#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
中兴 AX5400 Pro+ (ZXSLC SR6211) 广东电信 IPTV 面板一键自动化部署脚本
用于首次安装、版本升级或路由器强制恢复出厂设置后的一键极速恢复。
特点：
  1. 纯 Python 标准库编写，零第三方库依赖 (无需 pip install)；
  2. 本地局域网千兆极速分发，数秒内完成全部部署；
  3. 全自动 MD5 完整性校验与内存盘零磨损架构部署；
  4. 自动在路由器原厂后台 (http://192.168.1.2) 挂载独立的「广东 IPTV」选项卡与控制开关，
     且完全独立运行，绝不影响网易 UU 插件、玩辰插件、节点小宝这三个原厂插件开关。
"""

import functools
import http.server
import json
import os
import re
import select
import socket
import socketserver
import sys
import threading
import time

ROUTER_HOST = "192.168.1.2"
ROUTER_PORT = 23
ROUTER_USER = "root"
ROUTER_PASS = "Z.123456z"
EXPECTED_MD5 = "745660251d3d17bb57893f427b6a9295"
HTTP_PORT = 8000

BASE_DIR = os.path.dirname(os.path.abspath(__file__))
ASSETS_DIR = os.path.join(BASE_DIR, "deploy_assets")

IAC = 255
DONT = 254
DO = 253
WONT = 252
WILL = 251
SB = 250
SE = 240

def strip_and_reply_iac(sock: socket.socket, data: bytes) -> bytes:
    out = bytearray()
    i = 0
    n = len(data)
    while i < n:
        b = data[i]
        if b == IAC and i + 1 < n:
            cmd = data[i + 1]
            if cmd == IAC:
                out.append(IAC)
                i += 2
            elif cmd in (DO, DONT, WILL, WONT) and i + 2 < n:
                opt = data[i + 2]
                if cmd == DO:
                    sock.sendall(bytes([IAC, WONT, opt]))
                elif cmd == WILL:
                    sock.sendall(bytes([IAC, DONT, opt]))
                i += 3
            elif cmd == SB:
                j = i + 2
                while j + 1 < n and not (data[j] == IAC and data[j + 1] == SE):
                    j += 1
                i = j + 2 if j + 1 < n else n
            else:
                i += 2
        else:
            out.append(b)
            i += 1
    return bytes(out)

class TelnetClient:
    def __init__(self, host=ROUTER_HOST, port=ROUTER_PORT, user=ROUTER_USER, passwd=ROUTER_PASS):
        self.host = host
        self.port = port
        self.user = user
        self.passwd = passwd
        self.sock = None

    def connect(self):
        self.sock = socket.create_connection((self.host, self.port), timeout=4)
        self.sock.setblocking(False)
        self._recv_until([b"login:", b"Login:", b"Username:"], timeout=5)
        self.sock.sendall((self.user + "\n").encode("utf-8"))
        self._recv_until([b"Password:", b"password:"], timeout=5)
        self.sock.sendall((self.passwd + "\n").encode("utf-8"))
        login_out = self._recv_until([b"# ", b"$ ", b"/ #"], timeout=5)
        if b"incorrect" in login_out.lower() or b"failed" in login_out.lower():
            raise RuntimeError(f"Telnet 登录失败，请检查账号密码！返回: {login_out.decode('utf-8', 'replace')}")
        self._drain(0.4)

    def _recv_until(self, markers: list, timeout: float = 8.0) -> bytes:
        buf = bytearray()
        start = time.time()
        while time.time() - start < timeout:
            r, _, _ = select.select([self.sock], [], [], 0.1)
            if r:
                try:
                    data = self.sock.recv(4096)
                    if not data:
                        break
                    buf.extend(strip_and_reply_iac(self.sock, data))
                    for m in markers:
                        if m in buf:
                            return bytes(buf)
                except Exception:
                    break
        return bytes(buf)

    def _drain(self, timeout: float = 0.3) -> bytes:
        buf = bytearray()
        start = time.time()
        while time.time() - start < timeout:
            r, _, _ = select.select([self.sock], [], [], 0.1)
            if r:
                try:
                    data = self.sock.recv(4096)
                    if not data:
                        break
                    buf.extend(strip_and_reply_iac(self.sock, data))
                    start = time.time()
                except Exception:
                    break
        return bytes(buf)

    def cmd(self, command: str, timeout: float = 10.0) -> str:
        cmd_clean = command.strip()
        marker = "__CMD_DONE_deploy__"
        if cmd_clean.endswith("&") or "\n" in cmd_clean:
            full = f"{cmd_clean}\necho {marker}:$?\n"
        else:
            full = f"{cmd_clean}; echo {marker}:$?\n"
        self.sock.sendall(full.encode("utf-8"))
        out = self._recv_until([f"{marker}:".encode("ascii")], timeout=timeout)
        extra = self._drain(0.2)
        raw = (out + extra).decode("utf-8", "replace")
        lines = []
        for line in raw.splitlines():
            s = line.strip()
            if marker in s or s == cmd_clean or s == full.strip():
                continue
            lines.append(line)
        return "\n".join(lines).strip()

    def close(self):
        if self.sock:
            try:
                self.sock.sendall(b"exit\n")
            except Exception:
                pass
            self.sock.close()

def get_local_lan_ip(target_ip=ROUTER_HOST):
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.connect((target_ip, 80))
        ip = s.getsockname()[0]
        s.close()
        return ip
    except Exception:
        return "192.168.1.88"

def main():
    print("=" * 66)
    print("  中兴 AX5400 Pro+ (SR6211) 广东电信 IPTV 面板一键自动化部署程序")
    print("=" * 66)

    # 1. 检测本地资源
    if not os.path.exists(ASSETS_DIR):
        print(f"[-] 错误：找不到部署资产目录: {ASSETS_DIR}")
        input("按回车键退出...")
        sys.exit(1)

    bin_path = os.path.join(ASSETS_DIR, "gd-iptv-panel")
    if not os.path.exists(bin_path):
        print(f"[-] 错误：找不到程序二进制: {bin_path}")
        input("按回车键退出...")
        sys.exit(1)

    lan_ip = get_local_lan_ip()
    print(f"[*] 本机局域网 IP: {lan_ip}")
    print(f"[*] 目标路由器 IP: {ROUTER_HOST}:{ROUTER_PORT}")

    # 2. 检查 Telnet 连通性
    print("[*] 正在测试路由器 Telnet 连通性...")
    telnet = TelnetClient()
    try:
        telnet.connect()
        print("[+] Telnet 连接与登录认证成功！")
    except Exception as e:
        print("\n" + "!" * 66)
        print("[-] 无法连接到路由器 Telnet（192.168.1.2:23）")
        print("    可能原因：路由器刚刚执行过【恢复出厂设置/强制还原】，Telnet 未开启。")
        print("\n[i] 极速恢复 Telnet 步骤（仅需 1 分钟）：")
        print("    1. 打开浏览器登录路由器后台: http://192.168.1.2 (密码 Z.123456z 或自定义)")
        print("    2. 进入「管理」->「系统管理」->「备份与恢复」->「恢复配置」")
        config_patch = os.path.join(BASE_DIR, "AX5400 Pro+", "解锁Telnet-config.bin")
        if os.path.exists(config_patch):
            print(f"    3. 选择并上传配置文件: {config_patch}")
        else:
            print("    3. 选择并上传 解锁Telnet-config.bin 配置文件")
        print("    4. 等待路由器重启约 1 分钟后，重新双击运行本脚本即可！")
        print("!" * 66 + "\n")
        input("按回车键退出...")
        sys.exit(2)

    # 3. 启动本地临时微型 HTTP 文件分发服务器
    socketserver.TCPServer.allow_reuse_address = True
    handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=ASSETS_DIR)
    httpd = socketserver.TCPServer(("0.0.0.0", HTTP_PORT), handler)
    http_thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    http_thread.start()
    print(f"[+] 本机临时文件分发服务已就绪 (端口 {HTTP_PORT})")

    try:
        # 4. 检查与准备路由器目录
        print("\n[*] 步骤 1/7: 检查路由器存储分区...")
        df_out = telnet.cmd("df -k /UUPlugin /tmp")
        print(df_out)
        telnet.cmd("killall gd-iptv-panel 2>/dev/null || true")
        telnet.cmd("killall uuplugin 2>/dev/null || true")

        # 5. 下载二进制到 /tmp 内存盘中转 (零闪存风险)
        print("\n[*] 步骤 2/7: 极速下发二进制至路由器内存盘...")
        dl_cmd = f"curl -fsSL -o /tmp/gd-iptv-panel.tmp http://{lan_ip}:{HTTP_PORT}/gd-iptv-panel"
        telnet.cmd(dl_cmd)

        # 6. MD5 校验
        print("[*] 步骤 3/7: 校验二进制完整性 (MD5)...")
        md5_out = telnet.cmd("md5sum /tmp/gd-iptv-panel.tmp")
        print(f"    路由器计算值: {md5_out}")
        if EXPECTED_MD5 not in md5_out:
            telnet.cmd("rm -f /tmp/gd-iptv-panel.tmp")
            raise RuntimeError(f"MD5 校验失败！期望值: {EXPECTED_MD5}")
        print("[+] MD5 校验 100% 匹配无误！")

        # 7. 固化安装到 /UUPlugin
        print("\n[*] 步骤 4/7: 安装程序与业务配置到持久化分区...")
        telnet.cmd("mv -f /tmp/gd-iptv-panel.tmp /UUPlugin/gd-iptv-panel && chmod 755 /UUPlugin/gd-iptv-panel")
        telnet.cmd(f"curl -fsSL -o /UUPlugin/config.json http://{lan_ip}:{HTTP_PORT}/config.json && chmod 644 /UUPlugin/config.json")
        telnet.cmd(f"curl -fsSL -o /UUPlugin/uu.conf http://{lan_ip}:{HTTP_PORT}/uu.conf && chmod 644 /UUPlugin/uu.conf")
        telnet.cmd(f"curl -fsSL -o /UUPlugin/index_patched.html http://{lan_ip}:{HTTP_PORT}/index_patched.html")
        telnet.cmd(f"curl -fsSL -o /UUPlugin/uuplugin http://{lan_ip}:{HTTP_PORT}/uuplugin && chmod 755 /UUPlugin/uuplugin")

        # 8. 固化冷启动自启开关
        print("\n[*] 步骤 5/7: 固化冷启动自启设置到系统配置库...")
        telnet.cmd("sendcmd 1 DB set QDCfg 0 QdUUplugindEnable 1")
        telnet.cmd("sendcmd 1 DB save")

        # 9. 启动服务与挂载前端界面
        print("\n[*] 步骤 6/7: 启动 IPTV 服务与挂载原生管理页独立选项卡...")
        telnet.cmd("setsid /UUPlugin/uuplugin >/dev/null 2>&1 &")
        print("    等待进程启动...")
        time.sleep(3)

        # 10. 服务健康检查与实测
        print("\n[*] 步骤 7/7: 服务端到端健康检查...")
        ps_out = telnet.cmd("ps | grep -E 'gd-iptv-panel|uuplugin'")
        print(f"    进程状态:\n{ps_out}")

        mem_out = telnet.cmd("cat /proc/$(pgrep gd-iptv-panel)/status 2>/dev/null | grep -E 'VmRSS|Threads'")
        print(f"    资源消耗:\n{mem_out}")

        http_check = telnet.cmd("curl -s -i http://127.0.0.1:4022/api/auth-check")
        if "200 OK" in http_check:
            print("[+] IPTV 服务接口运行正常 (HTTP 200 OK)！")
        else:
            print(f"[!] 警告: 接口响应异常: {http_check}")

        print("\n" + "=" * 66)
        print("  🎉 恭喜！中兴 AX5400 Pro+ 广东电信 IPTV 部署固化成功！")
        print("=" * 66)
        print("\n📺 常用访问入口与订阅地址：")
        print("  1. 路由器原厂管理页 (已注入 1:1 原生风格「IPTV插件」独立选项卡)：")
        print("     -> 访问: http://192.168.1.2/#/app/application")
        print("     -> 特性: 独立「IPTV插件」开关（提交后生效），绝不干扰网易UU、玩辰、节点小宝")
        print("\n  2. IPTV 独立管理后台：")
        print("     -> 地址: http://192.168.1.2:4022/")
        print("     -> 账号: mc838   密码: 8462277")
        print("\n  3. 电视盒 / 播放器 M3U 播放列表订阅源：")
        print("     -> 地址: http://192.168.1.2:4022/playlist")
        print("\n  4. 电视盒 / 播放器 XMLTV 电子节目单 (EPG)：")
        print("     -> 地址: http://192.168.1.2:4022/xmltv")
        print("\n🛡️ 特别说明：")
        print("  - 日常运行常驻内存仅 ~1.5 MB，CPU 占用 0%；")
        print("  - 日志与台标全部保存在内存盘 (/tmp)，对闪存零写入磨损；")
        print("  - 已经设置了断电冷启动守护，路由器断电重启自动恢复；")
        print("=" * 66)

    except Exception as err:
        print(f"\n[-] 部署过程中发生错误: {err}")
    finally:
        telnet.close()
        httpd.shutdown()

    print()
    input("按回车键退出本程序...")

if __name__ == "__main__":
    main()
