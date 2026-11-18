# gd-iptv-panel

广东电信 IPTV 播放列表 + EPG + 内置 RTSP/UDP 代理面板，**专为低资源路由器（OpenWrt / ARM / x86）优化重构**。

- **极致轻量单二进制**：基于 `hyper` 1.x + 单线程 Tokio 运行时，无 `actix-web` / `reqwest` / `rustls` / `xml-rs` / `chrono` / `regex` 等重型依赖。
- **超低资源占用**：静态 musl 二进制体积小、单线程常驻低内存、EPG 流式输出（不整包缓存到内存）、支持 OpenWrt tmpfs 图标缓存不伤闪存。
- **开箱即用**：内置 UDP 组播（`/rtp/`）与 RTSP（`/rtsp/`）代理，支持出站网卡/VLAN 绑定，无需额外部署 `rtp2httpd`。

| 服务 | 地址 |
|---|---|
| 配置面板 | `http://<host>:4022/` |
| 播放列表 | `http://<host>:4022/playlist` |
| EPG (XMLTV) | `http://<host>:4022/xmltv` |

---

## 一、前置条件

- IPTV 账号需已由机顶盒注册并绑定真实 MAC。
- 部署设备（路由器 / 旁路由 / Linux 主机）需能访问 IPTV 专网并接收 IGMP 组播。
- 支持架构：`linux/amd64`、`linux/arm64`、`linux/armv7`（全静态 musl 二进制，无 glibc 依赖）。

---

## 二、安装部署

### 方式 A：一键脚本（自动识别 OpenWrt `procd` 与 `systemd`）

```sh
curl -fsSL https://raw.githubusercontent.com/yfzoyz/gd-iptv/main/install.sh | sh
```

常用管理命令：

```sh
./install.sh upgrade     # 升级（失败自动回滚）
./install.sh restart     # 重启
./install.sh status      # 状态
./install.sh logs        # 实时日志
./install.sh uninstall   # 卸载
```

> 在 OpenWrt 上，脚本会自动生成 `/etc/init.d/gd-iptv-panel`（procd），并将图标缓存指向 `/tmp/gd-iptv-panel/logo-cache`（内存盘），避免写入路由器闪存。

### 方式 B：ZXSLC SR6211 / 中兴只读根分区路由器（ARM64 / Linux 4.1.25 / BusyBox）

针对根分区只读、无 Swap、`/UUPlugin`（4.7 MB）或 `/status`（4.6 MB）空间紧张的路由器，推荐使用 `arm64-upx` 静态二进制，并将缓存与 PID 放到 `/tmp` 内存盘实现零闪存磨损：

```sh
mkdir -p /UUPlugin/gd-iptv-panel /tmp/gd-iptv-panel/logo-cache

# 下载 UPX 压缩版静态 ARM64 二进制（约 500~700 KB，无 glibc 依赖，兼容 Linux 4.1.25）
curl -fsSL -o /UUPlugin/gd-iptv-panel/gd-iptv-panel \
  https://github.com/yfzoyz/gd-iptv/releases/latest/download/gd-iptv-panel-linux-arm64-upx
chmod +x /UUPlugin/gd-iptv-panel/gd-iptv-panel

# 后台启动（配置文件存 NAND，图标缓存与 PID/日志存 tmpfs）
RUST_LOG=info,hyper=warn,retina=warn \
  /UUPlugin/gd-iptv-panel/gd-iptv-panel \
  -c /UUPlugin/gd-iptv-panel/config.json \
  -d /tmp/gd-iptv-panel/logo-cache \
  -p /tmp/gd-iptv-panel/gd-iptv-panel.pid >/tmp/gd-iptv-panel/gd-iptv-panel.log 2>&1 &
```

> 也可以直接运行 `install.sh`，脚本会自动识别 `/UUPlugin` / `/status`、兼容 BusyBox v1.17.2，并生成 `/UUPlugin/gd-iptv-panel/service.sh` 启停管理脚本。

### 方式 C：OpenWrt 手动运行

```sh
# 1. 按架构下载二进制（amd64 / arm64 / armv7）
wget -O /usr/bin/gd-iptv-panel https://github.com/yfzoyz/gd-iptv/releases/latest/download/gd-iptv-panel-linux-arm64
chmod +x /usr/bin/gd-iptv-panel

# 2. 前台试运行（首次自动生成配置文件）
/usr/bin/gd-iptv-panel -c /etc/gd-iptv-panel/config.json -d /tmp/gd-iptv-panel/logo-cache
```

### 方式 D：Docker（基于 `scratch` 极简镜像）

```bash
docker run -d \
  --name gd-iptv-panel \
  --network host \
  --restart unless-stopped \
  -v ./data:/data \
  ghcr.io/yfzoyz/gd-iptv-panel:latest
```

---

## 三、首次配置

打开 `http://<路由器IP>:4022/`，默认账号密码均为 `admin`。

1. **面板登录**：建议修改默认密码。
2. **IPTV 账号认证**：填写 `UserID`、密码、机顶盒 `MAC`；路由器多口 / VLAN 环境可在「出站网卡」填写 IPTV 接口名（如 `eth0.41`、`br-iptv`），认证、RTSP 与 IGMP 组播都会绑定到该接口。
3. **内置代理**：
   - 直播勾选 **UDP 组播代理**（走 `/rtp/`）
   - 回看勾选 **RTSP 代理 + 生成时移回看**
   - 按需填写当地 FCC 服务器地址（`IP:端口`）
4. 点击 **保存配置**，可用 **测试 IPTV 连接** 验证认证与频道数量。
5. 将 `http://<路由器IP>:4022/playlist` 导入播放器（TiviMate / VLC / PotPlayer / Kodi），EPG 会通过 `#EXTM3U x-tvg-url` 自动加载。

---

## 四、命令行参数

```text
gd-iptv-panel [选项]

  -c, --config <PATH>      配置文件路径 [默认: $IPTV_CONFIG 或 ./config.json]
  -b, --bind <ADDR>        监听地址（覆盖配置文件）
  -d, --cache-dir <DIR>    Logo 缓存目录（路由器建议 /tmp/...）
  -p, --pidfile <PATH>     PID 文件路径（适用于无 systemd 的嵌入式路由器）
      --healthcheck [ADDR] 健康检查（用于 Docker 容器）
  -V, --version            显示版本
  -h, --help               显示帮助
```

---

## 五、API 列表

| 方法 | 路径 | 认证 | 说明 |
|---|---|---|---|
| GET | `/` | ❌ | Web 配置面板 |
| POST | `/api/login` | ❌ | 登录获取 Token |
| POST | `/api/logout` | ✅ | 注销当前 Token |
| GET | `/api/auth-check` | ❌ | 校验 Token |
| GET | `/api/config` | ✅ | 读取配置 |
| POST | `/api/config` | ✅ | 保存配置 |
| GET | `/api/status` | ✅ | 服务状态与缓存统计 |
| POST | `/api/test` | ✅ | 测试 IPTV 认证并返回频道数 |
| POST | `/api/clear-logo-cache` | ✅ | 清空图标缓存 |
| GET | `/playlist` | ❌ | M3U 播放列表 |
| GET | `/xmltv` | ❌ | 流式 XMLTV EPG |
| GET | `/logo/{id}.png` | ❌ | 频道图标（磁盘缓存） |
| GET | `/rtsp/{tail}` | ❌ | RTSP 直播/回看代理 |
| GET | `/rtp/{addr}` | ❌ | UDP 组播代理 |

---

## 六、源码构建

```bash
cargo test
cargo build --release --target x86_64-unknown-linux-musl
```

---

## 七、免责声明

仅供个人学习研究使用，请遵守当地法律法规和运营商服务条款。
