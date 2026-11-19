#!/bin/sh
# IPTV Panel (gd-iptv-panel) stop inside JDXB container
killall gd-iptv-panel 2>/dev/null
pkill -f "start_iptv" 2>/dev/null

# 插件关闭时解绑，恢复官方原厂界面
if grep -q "index_patched.html" /proc/mounts; then
    umount -l /kmodule/home/httpd/public/jquery/static/index.html 2>/dev/null
fi

exit 0
