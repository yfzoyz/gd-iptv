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

> 在 OpenWrt 上，脚本会自动生成 `/etc/init.d/gd-iptv-panel`（procd），并将日志指向 `/tmp`（内存盘），避免频繁写入路由器闪存。

### 方式 B：ZXSLC SR6211 / 中兴 AX5400 Pro+ 路由器（ARM64 / Linux 4.1.25 / 一键自动化）

针对中兴 AX5400 Pro+（ZXSLC SR6211），提供纯 Python 零依赖的 Windows **一键自动化部署程序**：

1. **一键部署**：
   在电脑端直接双击运行 `一键部署.bat`（或 `python 一键部署.py`），脚本将自动完成：
   - 检查 Telnet 连通性并自动完成局域网极速文件下发；
   - 固化程序至 `/UUPlugin/` 分区（仅占 545 KB，剩余 >4MB）；
   - 内存盘零磨损架构：日志与运行临时文件全部分流在 `/tmp`；
   - **原厂后台独立选项卡与开关**：自动在路由器后台（`http://192.168.5.1/#/app/application`）挂载独立的「IPTV插件」选项卡与独立控制开关，且**完全独立，绝不影响网易 UU 插件、玩辰插件、节点小宝三个原厂开关**；
   - 固化冷启动自启机制，路由器重启断电自动恢复；
   - 自动化端到端健康检查并打印订阅地址。

2. **路由器强制还原/恢复出厂设置后的恢复流程**：
   - 第一步：打开浏览器登录路由器后台（出厂默认 `http://192.168.5.1`），在「系统管理」->「备份与恢复」->「恢复配置」中上传 `AX5400 Pro+\解锁Telnet-config.bin` 激活 Telnet；
   - 第二步：重启完成后，双击电脑上的 `一键部署.bat`，即可一键全自动恢复全部 IPTV 服务！

---

```sh
/usr/bin/gd-iptv-panel -c /etc/gd-iptv-panel/config.json
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
  -p, --pidfile <PATH>     PID 文件路径（适用于无 systemd 的嵌入式路由器）
      --healthcheck [ADDR] 健康检查（用于 Docker 容器）
  -V, --version            显示版本
  -h, --help               显示帮助
```

---

## 五、API 列表

| 方法 | 路径 | 认证 | 说明 |
|---|---|---|---|
| GET | `/` | ❌ | Web 配置面板（插件关闭时显示停用提示） |
| POST | `/api/login` | ❌ | 登录获取 Token |
| POST | `/api/logout` | ✅ | 注销当前 Token |
| GET | `/api/auth-check` | ❌ | 校验 Token |
| GET | `/api/config` | ✅ | 读取配置 |
| POST | `/api/config` | ✅ | 保存配置 |
| GET | `/api/status` | ✅ | 服务状态与基础统计 |
| POST | `/api/test` | ✅ | 测试 IPTV 认证并返回频道数 |
| GET | `/api/plugin/state` | ❌ | 供路由器原生管理页查询开关状态 |
| POST | `/api/plugin/enable`| ❌ | 供路由器原生管理页提交开启 |
| POST | `/api/plugin/disable`| ❌ | 供路由器原生管理页提交关闭 |
| GET | `/playlist` | ❌ | M3U 播放列表 |
| GET | `/xmltv` | ❌ | 流式 XMLTV EPG |
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
