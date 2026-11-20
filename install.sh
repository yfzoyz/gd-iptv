#!/bin/sh
#
# gd-iptv-panel 安装 / 升级 / 管理脚本
# 兼容：
#   1. 中兴等只读根分区嵌入式路由器（如 ZXSLC SR6211，BusyBox v1.17.2，安装至 /UUPlugin 或 /status）
#   2. OpenWrt (procd)
#   3. 标准 Linux (systemd)
#
# 环境变量（可选）：
#   REPO=yfzoyz/gd-iptv
#   INSTALL_DIR=/UUPlugin/gd-iptv-panel   # 仅嵌入式路由器模式生效
#   USE_UPX=1                             # 下载 UPX 压缩版二进制（更省闪存，约 500~700 KB）
#
# 用法:
#   ./install.sh            安装（若已存在则升级）
#   ./install.sh install    安装
#   ./install.sh upgrade    升级到最新版本
#   ./install.sh uninstall  卸载
#   ./install.sh restart    重启服务
#   ./install.sh status     查看状态
#   ./install.sh logs       查看日志
#   ./install.sh version    查看版本
#

set -e

REPO="${REPO:-yfzoyz/gd-iptv}"
BIN_NAME="gd-iptv-panel"
USE_UPX="${USE_UPX:-0}"

# 检测运行环境（不依赖 head / pgrep 等精简 BusyBox 缺失命令）
if [ -d /status ]; then
    INIT_TYPE="embedded"
    BASE_DIR="${INSTALL_DIR:-/status/${BIN_NAME}}"
    INSTALL_PATH="${BASE_DIR}/${BIN_NAME}"
    CONFIG_DIR="${BASE_DIR}"
    CONFIG_FILE="${CONFIG_DIR}/config.json"
    SERVICE_FILE="${BASE_DIR}/service.sh"
    RUNTIME_DIR="/tmp/${BIN_NAME}"
    CACHE_DIR="${RUNTIME_DIR}/logo-cache"
    PID_FILE="${RUNTIME_DIR}/${BIN_NAME}.pid"
    LOG_FILE="${RUNTIME_DIR}/${BIN_NAME}.log"
    # 小容量 NAND 分区默认启用 UPX 压缩版以节省闪存
    [ "$USE_UPX" = "0" ] && USE_UPX="1"
elif [ -f /etc/openwrt_release ] || [ -x /sbin/procd ]; then
    INIT_TYPE="procd"
    INSTALL_PATH="/usr/bin/${BIN_NAME}"
    CONFIG_DIR="/etc/${BIN_NAME}"
    CONFIG_FILE="${CONFIG_DIR}/config.json"
    SERVICE_FILE="/etc/init.d/${BIN_NAME}"
    CACHE_DIR="/tmp/${BIN_NAME}/logo-cache"
elif command -v systemctl >/dev/null 2>&1; then
    INIT_TYPE="systemd"
    INSTALL_PATH="/usr/local/bin/${BIN_NAME}"
    CONFIG_DIR="/etc/${BIN_NAME}"
    CONFIG_FILE="${CONFIG_DIR}/config.json"
    SERVICE_NAME="${BIN_NAME}.service"
    SERVICE_FILE="/etc/systemd/system/${SERVICE_NAME}"
    CACHE_DIR="${CONFIG_DIR}/logo-cache"
else
    INIT_TYPE="embedded"
    BASE_DIR="${INSTALL_DIR:-/tmp/${BIN_NAME}-app}"
    INSTALL_PATH="${BASE_DIR}/${BIN_NAME}"
    CONFIG_DIR="${BASE_DIR}"
    CONFIG_FILE="${CONFIG_DIR}/config.json"
    SERVICE_FILE="${BASE_DIR}/service.sh"
    RUNTIME_DIR="/tmp/${BIN_NAME}"
    CACHE_DIR="${RUNTIME_DIR}/logo-cache"
    PID_FILE="${RUNTIME_DIR}/${BIN_NAME}.pid"
    LOG_FILE="${RUNTIME_DIR}/${BIN_NAME}.log"
fi

info() { printf '[INFO] %s\n' "$*"; }
ok()   { printf '[ OK ] %s\n' "$*"; }
warn() { printf '[WARN] %s\n' "$*"; }
err()  { printf '[FAIL] %s\n' "$*" >&2; }

need_root() {
    if [ "$(id -u)" -ne 0 ]; then
        err "此操作需要 root 权限"
        exit 1
    fi
}

has_cmd() { command -v "$1" >/dev/null 2>&1; }

detect_arch() {
    arch="$(uname -m)"
    case "$arch" in
        x86_64|amd64)  echo "amd64" ;;
        aarch64|arm64) echo "arm64" ;;
        armv7l|armv7*) echo "armv7" ;;
        *)
            err "不支持的架构：$arch（支持 amd64 / arm64 / armv7）"
            exit 1
            ;;
    esac
}

download_file() {
    url="$1"
    dest="$2"
    if has_cmd curl; then
        curl -fsSL -o "$dest" "$url"
    elif has_cmd wget; then
        wget -q -O "$dest" "$url"
    else
        err "需要 curl 或 wget 才能下载"
        exit 1
    fi
}

get_latest_version() {
    url="https://api.github.com/repos/${REPO}/releases/latest"
    if has_cmd curl; then
        curl -fsSL "$url" 2>/dev/null | grep '"tag_name"' | sed -n '1p' | sed 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/'
    elif has_cmd wget; then
        wget -qO- "$url" 2>/dev/null | grep '"tag_name"' | sed -n '1p' | sed 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/'
    fi
}

get_port() {
    if [ -f "$CONFIG_FILE" ]; then
        p="$(grep '"bind"' "$CONFIG_FILE" 2>/dev/null | sed -n '1p' | sed 's/.*:\([0-9][0-9]*\)".*/\1/')"
        [ -n "$p" ] && echo "$p" || echo "4022"
    else
        echo "4022"
    fi
}

download_binary() {
    arch="$1"
    out="$2"
    suffix=""
    [ "$USE_UPX" = "1" ] && suffix="-upx"
    file="${BIN_NAME}-linux-${arch}${suffix}"
    url="https://github.com/${REPO}/releases/latest/download/${file}"
    info "下载 ${file}: ${url}"
    download_file "$url" "$out" || {
        if [ -n "$suffix" ]; then
            warn "UPX 版下载失败，回退标准静态二进制"
            file="${BIN_NAME}-linux-${arch}"
            url="https://github.com/${REPO}/releases/latest/download/${file}"
            download_file "$url" "$out" || { err "下载失败"; exit 1; }
        else
            err "下载失败"
            exit 1
        fi
    }
    if [ ! -s "$out" ]; then
        err "下载的文件为空"
        exit 1
    fi
    chmod +x "$out"

    if has_cmd sha256sum; then
        tmp_sha="/tmp/${BIN_NAME}.sha256.$$"
        if download_file "${url}.sha256" "$tmp_sha" 2>/dev/null; then
            expected="$(awk '{print $1}' "$tmp_sha")"
            actual="$(sha256sum "$out" | awk '{print $1}')"
            if [ "$expected" = "$actual" ]; then
                ok "SHA256 校验通过"
            else
                rm -f "$tmp_sha"
                err "SHA256 校验失败（expected=$expected actual=$actual）"
                exit 1
            fi
        fi
        rm -f "$tmp_sha"
    fi
}

create_service() {
    if [ "$INIT_TYPE" = "embedded" ]; then
        info "创建嵌入式路由器管理脚本：${SERVICE_FILE}"
        cat > "$SERVICE_FILE" <<EOF
#!/bin/sh
BIN="${INSTALL_PATH}"
CFG="${CONFIG_FILE}"
CACHE="${CACHE_DIR}"
PID="${PID_FILE}"
LOG="${LOG_FILE}"

is_running() {
    [ -f "\$PID" ] || return 1
    p="\$(cat "\$PID" 2>/dev/null)"
    [ -n "\$p" ] && kill -0 "\$p" 2>/dev/null
}

start() {
    mkdir -p "${RUNTIME_DIR}" "\$CACHE"
    if is_running; then
        echo "${BIN_NAME} 已在运行 (PID \$(cat "\$PID"))"
        return 0
    fi
    RUST_LOG=info,hyper=warn,retina=warn "\$BIN" -c "\$CFG" -d "\$CACHE" -p "\$PID" >>"\$LOG" 2>&1 &
    sleep 1
    if is_running; then
        echo "${BIN_NAME} 启动成功 (PID \$(cat "\$PID"))"
    else
        echo "${BIN_NAME} 启动失败，日志：\$LOG" >&2
        return 1
    fi
}

stop() {
    if is_running; then
        kill "\$(cat "\$PID")" 2>/dev/null || true
        sleep 1
    fi
    rm -f "\$PID"
}

case "\${1:-start}" in
    start)   start ;;
    stop)    stop ;;
    restart) stop; start ;;
    status)  is_running && echo "running (PID \$(cat "\$PID"))" || echo "stopped" ;;
    logs)    [ -f "\$LOG" ] && tail -f "\$LOG" || echo "暂无日志" ;;
    *)       echo "用法: \$0 {start|stop|restart|status|logs}" ;;
esac
EOF
        chmod +x "$SERVICE_FILE"
        ok "管理脚本已创建：${SERVICE_FILE}（图标缓存与日志位于 tmpfs：${RUNTIME_DIR}）"
    elif [ "$INIT_TYPE" = "procd" ]; then
        info "创建 OpenWrt procd 服务：${SERVICE_FILE}"
        cat > "$SERVICE_FILE" <<EOF
#!/bin/sh /etc/rc.common

START=95
STOP=10
USE_PROCD=1

start_service() {
    mkdir -p "${CONFIG_DIR}"
    procd_open_instance
    procd_set_param command "${INSTALL_PATH}" -c "${CONFIG_FILE}"
    procd_set_param env RUST_LOG=info,hyper=warn,retina=warn
    procd_set_param respawn 3600 5 5
    procd_set_param stdout 1
    procd_set_param stderr 1
    procd_close_instance
}
EOF
        chmod +x "$SERVICE_FILE"
        "$SERVICE_FILE" enable
        ok "OpenWrt 服务已创建并设置开机自启"
    elif [ "$INIT_TYPE" = "systemd" ]; then
        info "创建 systemd 服务：${SERVICE_FILE}"
        cat > "$SERVICE_FILE" <<EOF
[Unit]
Description=gd-iptv-panel
Documentation=https://github.com/${REPO}
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=${INSTALL_PATH} -c ${CONFIG_FILE}
Restart=on-failure
RestartSec=5
Environment=RUST_LOG=info,hyper=warn,retina=warn

[Install]
WantedBy=multi-user.target
EOF
        systemctl daemon-reload
        systemctl enable "$SERVICE_NAME" >/dev/null 2>&1
        ok "systemd 服务已创建并启用"
    fi
}

service_stop() {
    if [ "$INIT_TYPE" = "embedded" ] && [ -x "$SERVICE_FILE" ]; then
        "$SERVICE_FILE" stop 2>/dev/null || true
    elif [ "$INIT_TYPE" = "procd" ] && [ -x "$SERVICE_FILE" ]; then
        "$SERVICE_FILE" stop 2>/dev/null || true
    elif [ "$INIT_TYPE" = "systemd" ]; then
        systemctl stop "$SERVICE_NAME" 2>/dev/null || true
    fi
    if has_cmd killall; then
        killall "$BIN_NAME" 2>/dev/null || true
    fi
}

service_start() {
    if [ "$INIT_TYPE" = "embedded" ]; then
        "$SERVICE_FILE" restart
    elif [ "$INIT_TYPE" = "procd" ]; then
        "$SERVICE_FILE" restart
    elif [ "$INIT_TYPE" = "systemd" ]; then
        systemctl restart "$SERVICE_NAME"
    fi
}

service_running() {
    if [ "$INIT_TYPE" = "embedded" ]; then
        [ -f "$PID_FILE" ] || return 1
        p="$(cat "$PID_FILE" 2>/dev/null)"
        [ -n "$p" ] && kill -0 "$p" 2>/dev/null
    elif [ "$INIT_TYPE" = "procd" ]; then
        pidof "$BIN_NAME" >/dev/null 2>&1
    elif [ "$INIT_TYPE" = "systemd" ]; then
        systemctl is-active --quiet "$SERVICE_NAME"
    else
        return 0
    fi
}

cmd_install() {
    need_root
    arch="$(detect_arch)"
    info "检测到环境：${INIT_TYPE} / 架构：${arch} / UPX：${USE_UPX}"

    tmp_bin="/tmp/${BIN_NAME}.bin.$$"
    trap 'rm -f "$tmp_bin"' EXIT
    download_binary "$arch" "$tmp_bin"

    service_stop
    mkdir -p "$(dirname "$INSTALL_PATH")" "$CONFIG_DIR"
    chmod 700 "$CONFIG_DIR" 2>/dev/null || true
    cp "$tmp_bin" "$INSTALL_PATH"
    chmod 755 "$INSTALL_PATH"
    ok "二进制已安装至 ${INSTALL_PATH}"

    create_service
    service_start
    sleep 1

    if service_running; then
        port="$(get_port)"
        ok "服务已启动，默认面板地址：http://<设备IP>:${port}/（默认账号 admin / admin）"
    else
        err "服务启动失败，请查看日志"
        exit 1
    fi
}

cmd_upgrade() {
    need_root
    if [ ! -f "$INSTALL_PATH" ]; then
        err "尚未安装，请先运行：$0 install"
        exit 1
    fi
    old_ver="$("$INSTALL_PATH" --version 2>/dev/null || echo unknown)"
    info "当前版本：${old_ver}"

    arch="$(detect_arch)"
    tmp_bin="/tmp/${BIN_NAME}.bin.$$"
    trap 'rm -f "$tmp_bin"' EXIT
    download_binary "$arch" "$tmp_bin"

    # 备份放 /tmp 避免占用小容量 NAND 闪存
    bak="/tmp/${BIN_NAME}.bak.$$"
    cp "$INSTALL_PATH" "$bak"
    service_stop
    cp "$tmp_bin" "$INSTALL_PATH"
    chmod 755 "$INSTALL_PATH"
    service_start
    sleep 1

    if service_running; then
        rm -f "$bak"
        new_ver="$("$INSTALL_PATH" --version 2>/dev/null || echo unknown)"
        ok "升级成功：${old_ver} -> ${new_ver}"
    else
        err "升级后启动失败，正在回滚"
        cp "$bak" "$INSTALL_PATH"
        chmod 755 "$INSTALL_PATH"
        rm -f "$bak"
        service_start
        exit 1
    fi
}

cmd_uninstall() {
    need_root
    service_stop
    if [ "$INIT_TYPE" = "embedded" ]; then
        rm -f "$SERVICE_FILE" "$INSTALL_PATH"
        rm -rf "$RUNTIME_DIR"
    elif [ "$INIT_TYPE" = "procd" ] && [ -x "$SERVICE_FILE" ]; then
        "$SERVICE_FILE" disable 2>/dev/null || true
        rm -f "$SERVICE_FILE" "$INSTALL_PATH"
    elif [ "$INIT_TYPE" = "systemd" ]; then
        systemctl disable "$SERVICE_NAME" 2>/dev/null || true
        rm -f "$SERVICE_FILE" "$INSTALL_PATH"
        systemctl daemon-reload
    fi
    ok "已卸载二进制与服务（配置文件保留在 ${CONFIG_DIR}）"
}

cmd_status() {
    if [ "$INIT_TYPE" = "embedded" ]; then
        [ -x "$SERVICE_FILE" ] && "$SERVICE_FILE" status || warn "未安装"
    elif [ "$INIT_TYPE" = "procd" ]; then
        service_running && ok "运行中" || warn "未运行"
    elif [ "$INIT_TYPE" = "systemd" ]; then
        systemctl status "$SERVICE_NAME" --no-pager -l
    fi
}

cmd_logs() {
    if [ "$INIT_TYPE" = "embedded" ]; then
        [ -x "$SERVICE_FILE" ] && "$SERVICE_FILE" logs || warn "未安装"
    elif [ "$INIT_TYPE" = "procd" ]; then
        logread -f -e "$BIN_NAME"
    elif [ "$INIT_TYPE" = "systemd" ]; then
        journalctl -u "$SERVICE_NAME" -f
    fi
}

cmd_version() {
    if [ ! -f "$INSTALL_PATH" ]; then
        err "未安装"
        exit 1
    fi
    printf '已安装版本：'
    "$INSTALL_PATH" --version 2>/dev/null || echo "unknown"
    latest="$(get_latest_version || true)"
    [ -n "$latest" ] && printf '最新版本：  %s\n' "$latest"
}

case "${1:-install}" in
    install)              cmd_install ;;
    upgrade|update)       cmd_upgrade ;;
    uninstall|remove)     cmd_uninstall ;;
    restart)              need_root; service_start; cmd_status ;;
    status)               cmd_status ;;
    logs|log)             cmd_logs ;;
    version|-V|--version) cmd_version ;;
    *)
        echo "用法: $0 {install|upgrade|uninstall|restart|status|logs|version}"
        exit 1
        ;;
esac
