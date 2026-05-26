#!/usr/bin/env python3
"""
StreamHub Launcher - Puerto 8081
Gestiona server.py y sirve el HTML.
Uso: python launcher.py
"""

import http.server
import json
import os
import socket
import subprocess
import sys
import threading
import time
from urllib.parse import urlparse, parse_qs

HOST    = "localhost"
PORT    = 8081
BASE    = os.path.dirname(os.path.abspath(__file__))
SRV_PY  = os.path.join(BASE, "server.py")
ALLOWED_ORIGINS = {
    f"http://{HOST}:{PORT}",
    "http://localhost:8080",
    "http://127.0.0.1:8080",
    "http://127.0.0.1:8081",
}
STATIC_FILES = {
    "/": "StreamHub.html",
    "/index": "StreamHub.html",
    "/index.html": "StreamHub.html",
    "/StreamHub.html": "StreamHub.html",
    "/twitch-multistream.html": "StreamHub.html",
    "/logo.png": "logo.png",
}

_srv_proc = None
_srv_lock = threading.Lock()


# ── PROCESO SERVER ─────────────────────────────────────────────

def srv_running():
    with _srv_lock:
        return _srv_proc is not None and _srv_proc.poll() is None

def start_server():
    global _srv_proc
    with _srv_lock:
        if _srv_proc and _srv_proc.poll() is None:
            return False, "Ya está corriendo"
        try:
            _srv_proc = subprocess.Popen(
                [sys.executable, SRV_PY],
                cwd=BASE,
                creationflags=subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0
            )
            time.sleep(0.5)
            return True, "Servidor iniciado"
        except Exception as e:
            return False, str(e)

def stop_server():
    global _srv_proc
    with _srv_lock:
        if _srv_proc is None or _srv_proc.poll() is not None:
            return False, "No estaba corriendo"
        try:
            if sys.platform == "win32":
                subprocess.run(
                    ["taskkill", "/F", "/T", "/PID", str(_srv_proc.pid)],
                    capture_output=True,
                    creationflags=subprocess.CREATE_NO_WINDOW
                )
            else:
                _srv_proc.terminate()
                _srv_proc.wait(timeout=5)
            _srv_proc = None
            return True, "Servidor detenido"
        except Exception as e:
            _srv_proc = None
            return False, str(e)

def restart_server():
    stop_server()
    time.sleep(0.8)
    return start_server()


# ── HTTP HANDLER ────────────────────────────────────────────────

class LaunchHandler(http.server.BaseHTTPRequestHandler):

    def log_message(self, fmt, *args):
        pass  # silencio

    def cors_origin(self):
        origin = self.headers.get("Origin")
        if origin in ALLOWED_ORIGINS:
            return origin
        return f"http://{HOST}:{PORT}" if not origin else None

    def add_cors_headers(self):
        origin = self.cors_origin()
        if origin:
            self.send_header("Access-Control-Allow-Origin", origin)
            self.send_header("Vary", "Origin")

    def origin_allowed(self):
        origin = self.headers.get("Origin")
        return not origin or origin in ALLOWED_ORIGINS

    def send_json(self, data, code=200):
        body = json.dumps(data).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.add_cors_headers()
        self.end_headers()
        try:
            self.wfile.write(body)
        except Exception:
            pass

    def send_file(self, path, mime="text/html"):
        real = os.path.realpath(path)
        if os.path.commonpath([BASE, real]) != BASE or not os.path.isfile(real):
            self.send_response(404)
            self.end_headers()
            return
        try:
            with open(real, "rb") as f:
                body = f.read()
            self.send_response(200)
            self.send_header("Content-Type", mime + "; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.add_cors_headers()
            self.end_headers()
            self.wfile.write(body)
        except FileNotFoundError:
            self.send_response(404)
            self.end_headers()

    def do_OPTIONS(self):
        if not self.origin_allowed():
            self.send_response(403)
            self.end_headers()
            return
        self.send_response(204)
        self.add_cors_headers()
        self.send_header("Access-Control-Allow-Methods", "GET,POST,OPTIONS")
        self.send_header("Access-Control-Allow-Headers", "Content-Type")
        self.end_headers()

    def do_GET(self):
        if not self.origin_allowed():
            self.send_json({"ok": False, "error": "Origen no permitido"}, 403)
            return
        parsed = urlparse(self.path)
        path   = parsed.path.rstrip("/") or "/"

        if path in STATIC_FILES:
            self.send_file(os.path.join(BASE, STATIC_FILES[path]))
        elif path == "/launcher/status":
            self.send_json({"running": srv_running()})
        else:
            self.send_response(404)
            self.end_headers()

    def do_POST(self):
        if not self.origin_allowed():
            self.send_json({"ok": False, "error": "Origen no permitido"}, 403)
            return
        parsed = urlparse(self.path)
        path   = parsed.path

        if path == "/launcher/start":
            ok, msg = start_server()
            self.send_json({"ok": ok, "msg": msg})
        elif path == "/launcher/stop":
            ok, msg = stop_server()
            self.send_json({"ok": ok, "msg": msg})
        elif path == "/launcher/restart":
            ok, msg = restart_server()
            self.send_json({"ok": ok, "msg": msg})
        else:
            self.send_json({"ok": False, "error": "Ruta no encontrada"}, 404)


# ── MAIN ────────────────────────────────────────────────────────

class ReuseServer(http.server.ThreadingHTTPServer):
    allow_reuse_address = True

def _port_in_use(port):
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.settimeout(0.5)
        return s.connect_ex(("127.0.0.1", port)) == 0

def main():
    os.chdir(BASE)
    print("\n  +==========================================+")
    print("  |      StreamHub Launcher                 |")
    print("  +==========================================+")

    for p in (8080, PORT):
        if _port_in_use(p):
            print(f"\n  ERROR: Puerto {p} ya está en uso.")
            print(f"  Cierra el proceso que lo ocupa e intenta de nuevo.")
            sys.exit(1)

    ok, msg = start_server()
    if ok:
        print(f"  ✓ server.py iniciado (PID {_srv_proc.pid})")
    else:
        print(f"  ! server.py: {msg}")

    server = ReuseServer((HOST, PORT), LaunchHandler)
    t = threading.Thread(target=server.serve_forever, daemon=True)
    t.start()
    print(f"  Launcher:  http://{HOST}:{PORT}")
    print(f"  App:       http://{HOST}:{PORT}/")
    print(f"  Ctrl+C para detener todo\n")

    try:
        while t.is_alive():
            t.join(timeout=0.5)
    except KeyboardInterrupt:
        print("\n  Cerrando...")
        stop_server()
        os._exit(0)

if __name__ == "__main__":
    main()
