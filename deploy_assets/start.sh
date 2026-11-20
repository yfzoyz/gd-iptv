#!/bin/sh
cd "${0%/*}"
BIN_DIR="$(pwd)"
PID_FILE="/owjdxbplugin/tmp/iptv.pid"

if [ -f "$PID_FILE" ]; then
    PID=$(cat "$PID_FILE" 2>/dev/null)
    if [ -n "$PID" ] && [ -d "/proc/$PID" ]; then
        exit 0
    fi
fi

if [ ! -e "${BIN_DIR}/owjdxb" ]; then
    ln "${BIN_DIR}/gd-iptv-panel" "${BIN_DIR}/owjdxb" 2>/dev/null || cp -p "${BIN_DIR}/gd-iptv-panel" "${BIN_DIR}/owjdxb"
fi

setsid ./owjdxb -c "${BIN_DIR}/config.json" >/dev/null 2>&1 &
echo $! > "$PID_FILE"

exit 0
