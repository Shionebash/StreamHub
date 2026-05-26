#!/usr/bin/env bash
# StreamHub - Linux launcher
set -e

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]:-$0}")" && pwd -P)"
cd "$SCRIPT_DIR"

if [ -x ".venv/bin/python" ]; then
    export PATH="$SCRIPT_DIR/.venv/bin:$PATH"
    PYTHON=".venv/bin/python"
elif command -v python3 >/dev/null 2>&1; then
    PYTHON="python3"
else
    PYTHON="python"
fi

URL="http://localhost:8080/twitch-multistream.html"
if command -v xdg-open >/dev/null 2>&1; then
    xdg-open "$URL" >/dev/null 2>&1 &
elif command -v firefox >/dev/null 2>&1; then
    firefox "$URL" >/dev/null 2>&1 &
elif command -v chromium >/dev/null 2>&1; then
    chromium "$URL" >/dev/null 2>&1 &
elif command -v google-chrome >/dev/null 2>&1; then
    google-chrome "$URL" >/dev/null 2>&1 &
fi

exec "$PYTHON" server.py
