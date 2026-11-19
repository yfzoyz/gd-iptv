#!/bin/sh
# IPTV Panel (gd-iptv-panel) autostart inside JDXB container
BIN_DIR="/jdxb/apps/owjdxbplugin/bin/owjdxb"

# 确保开机自启或插件开启时自动挂载修复后的原生前端界面
if [ -f /status/index_patched.html ] && ! grep -q "index_patched.html" /proc/mounts; then
    mount --bind /status/index_patched.html /kmodule/home/httpd/public/jquery/static/index.html 2>/dev/null
fi

start_iptv() {
    if ! pgrep gd-iptv-panel >/dev/null 2>&1; then
        setsid ${BIN_DIR}/gd-iptv-panel -c ${BIN_DIR}/config.json >/dev/null 2>&1 &
    fi
}

start_iptv

# 后台守护进程（每 30 秒监测一次，崩溃自动拉起）
(
    while true; do
        sleep 30
        start_iptv
    done
) >/dev/null 2>&1 &

exit 0
