#!/bin/sh
# IPTV Plugin Host Service & UI Mount (Replaces Owdpts)

# 1. 宿主机挂载前端补丁
PATCH=/status/index_patched.html
TARGET=/kmodule/home/httpd/public/jquery/static/index.html
if [ -f "$PATCH" ] && [ -f "$TARGET" ]; then
    if ! grep -q "$PATCH" /proc/mounts 2>/dev/null; then
        mount --bind "$PATCH" "$TARGET"
        echo "[$(date)] UI patch bind-mounted" > /tmp/ui_patch.log
    fi
fi

# 2. 宿主机启动 gd-iptv-panel (端口 4022)
IPTV_DIR=/status/iptv
PIDFILE=/tmp/iptv.pid
if [ -x "$IPTV_DIR/gd-iptv-panel" ] && [ -f "$IPTV_DIR/config.json" ]; then
    if [ -f "$PIDFILE" ] && kill -0 $(cat "$PIDFILE" 2>/dev/null) 2>/dev/null; then
        exit 0
    fi
    cd "$IPTV_DIR"
    setsid ./gd-iptv-panel -c config.json >/dev/null 2>&1 &
    echo $! > "$PIDFILE"
    echo "[$(date)] IPTV panel started on host with PID $!" > /tmp/iptv.log
fi
exit 0
