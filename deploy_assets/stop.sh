#!/bin/sh
# Stop IPTV service
PIDFILE=/tmp/iptv.pid
if [ -f "$PIDFILE" ]; then
    PID=$(cat "$PIDFILE" 2>/dev/null)
    [ -n "$PID" ] && kill "$PID" 2>/dev/null
    rm -f "$PIDFILE"
fi
killall gd-iptv-panel 2>/dev/null
echo "[$(date)] IPTV panel stopped" > /tmp/iptv.log
exit 0
