#!/usr/bin/env python3
"""
StreamHub Server - con favoritos y estado en vivo
Uso: python server.py
"""

import atexit
import http.server
import json
import logging
import logging.handlers
import os
import re
import secrets
import shutil
import subprocess
import sys
import threading
import time
import urllib.request
import urllib.parse
from concurrent.futures import ThreadPoolExecutor, as_completed
from urllib.parse import urlparse, parse_qs

# Módulos locales de mining (se importan con try para no romper si faltan)
try:
    from pubsub import PubSubManager, WEBSOCKET_AVAILABLE
except ImportError:
    PubSubManager        = None
    WEBSOCKET_AVAILABLE  = False
try:
    from twitch_gql import (
        claim_channel_points  as gql_claim_points,
        claim_drop            as gql_claim_drop,
        get_points_balance    as gql_get_balance,
        get_inventory_drops   as gql_get_inventory,
        get_channel_id        as gql_get_channel_id,
        get_stream_watch_info as gql_get_watch_info,
        send_minute_watched   as gql_send_watch,
        get_last_error        as gql_get_last_error,
        is_token_expired      as gql_is_token_expired,
        reset_token_expired   as gql_reset_token_expired,
        set_gql_client        as gql_set_client,
    )
    GQL_AVAILABLE = True
except ImportError:
    GQL_AVAILABLE = False
    gql_get_last_error    = lambda: ""
    gql_is_token_expired  = lambda: False
    gql_reset_token_expired = lambda: None
    gql_set_client        = lambda _: None

try:
    from secure_token import store_token, load_token, delete_token, migrate_from_plaintext
    SECURE_TOKEN_AVAILABLE = True
except ImportError:
    SECURE_TOKEN_AVAILABLE = False
    def store_token(t): return "none"
    def load_token(): return "", "none"
    def delete_token(): pass
    def migrate_from_plaintext(t): return False

HOST = "localhost"
PORT = 8080
BASE = os.path.dirname(os.path.abspath(__file__))
LOG_STREAMS_DIR = os.path.join(BASE, "logs streams")
LOG_APP_DIR     = os.path.join(BASE, "logs app")
RUNTIME_APP_DIR = os.path.join(BASE, "runtime app")
def _load_or_create_csrf():
    """Load persisted CSRF token so it survives server restarts."""
    _csrf_path = os.path.join(RUNTIME_APP_DIR, "csrf.token")
    os.makedirs(RUNTIME_APP_DIR, exist_ok=True)
    if os.path.exists(_csrf_path):
        try:
            with open(_csrf_path, "r", encoding="utf-8") as f:
                tok = f.read().strip()
            if len(tok) >= 32:
                return tok
        except Exception:
            pass
    tok = secrets.token_urlsafe(32)
    try:
        with open(_csrf_path, "w", encoding="utf-8") as f:
            f.write(tok)
        if sys.platform != "win32":
            os.chmod(_csrf_path, 0o600)
    except Exception:
        pass
    return tok

CSRF_TOKEN = _load_or_create_csrf()
ALLOWED_ORIGINS = {
    f"http://{HOST}:{PORT}",
    "http://localhost:8081",
    "http://127.0.0.1:8080",
    "http://127.0.0.1:8081",
}
STATIC_FILES = {
    "/": "twitch-multistream.html",
    "/twitch-multistream.html": "twitch-multistream.html",
    "/index.html": "twitch-multistream.html",
    "/logo.png": "logo.png",
}
CHANNEL_RE = re.compile(r"^[a-z0-9_]{3,25}$")
QUALITY_RE = re.compile(r"^(best|worst|audio_only|source|[0-9]{3,4}p60?|[0-9]{3,4}p)$")
PLAYERS = {"mpv", "vlc", "gridplayer"}
HW_GPUS = {"nvidia", "amd", "intel"}

procesos  = {}
grid_proc = None
lock      = threading.Lock()

# ── ESTADO MINING ─────────────────────────────────────────────
mining_lock    = threading.RLock()
points_state   = {}   # {channel_login: {balance, last_claim_ts, claim_count, session_earned}}
drops_state    = {}   # {drop_id: {...}}
mining_log     = []   # lista de dicts (max 100 eventos)
pubsub_mgr     = None
_balance_timer = None
_channel_id_cache = {}  # {channel_id: channel_login} — resuelto via Helix

# ── ESTADO WATCH ──────────────────────────────────────────────
watch_lock  = threading.Lock()
watch_state = {}   # {canal: {enabled, timer, channel_id, broadcast_id, game_id, user_id, ticks, last_ts}}

CONFIG_FILE    = os.path.join(BASE, "config.json")
URL_CACHE_FILE = os.path.join(RUNTIME_APP_DIR, "url_cache.json")
VLM_CONF       = os.path.join(RUNTIME_APP_DIR, "mosaic-vlm.conf")
CSRF_FILE      = os.path.join(RUNTIME_APP_DIR, "csrf.token")
URL_CACHE_TTL  = 12 * 60   # 12 minutos (HLS URLs expiran en ~15-60 min)
LOG_CLEANUP_INTERVAL = 5
TWITCH_API_CACHE_TTL = {
    "live": 20,
    "users": 5 * 60,
    "followed": 45,
    "categories": 5 * 60,
    "recommended": 45,
    "search": 2 * 60,
    "cat_streams": 45,
}
_twitch_api_cache = {}
_twitch_api_cache_lock = threading.Lock()
_last_log_cleanup = 0

SAFE_CONFIG_KEYS = {
    "quality", "clientId", "player", "mpv", "vlc", "gp", "sl", "ff",
    "w", "h", "hwaccel", "hwgpu", "vlcFfmpeg", "textOnly", "accentColor", "miningEnabled"
}

def is_allowed_origin(origin):
    return not origin or origin in ALLOWED_ORIGINS

def _copy_jsonable(value):
    return json.loads(json.dumps(value, ensure_ascii=False))

def cached_twitch_api(key, ttl, producer):
    now = time.time()
    with _twitch_api_cache_lock:
        hit = _twitch_api_cache.get(key)
        if hit and now - hit["ts"] < ttl:
            return _copy_jsonable(hit["value"])
    value = producer()
    with _twitch_api_cache_lock:
        _twitch_api_cache[key] = {"ts": now, "value": _copy_jsonable(value)}
    return value

def normalize_channel(value):
    channel = str(value or "").strip().lower()
    channel = channel.replace("https://", "").replace("http://", "")
    channel = channel.replace("www.", "").replace("twitch.tv/", "").lstrip("@")
    channel = channel.split("/")[0].split("?")[0]
    return channel if CHANNEL_RE.fullmatch(channel) else ""

def normalize_channels(values, limit=100):
    if not isinstance(values, list):
        return []
    result = []
    for value in values[:limit]:
        channel = normalize_channel(value)
        if channel and channel not in result:
            result.append(channel)
    return result

def normalize_quality(value):
    quality = str(value or "best").strip().lower()
    return quality if QUALITY_RE.fullmatch(quality) else "best"

def clamp_int(value, default, min_value, max_value):
    try:
        n = int(value)
    except (TypeError, ValueError):
        return default
    return max(min_value, min(max_value, n))

def normalize_player(value):
    player = str(value or "mpv").strip().lower()
    return player if player in PLAYERS else "mpv"

def normalize_path_value(value, default):
    text = str(value or "").strip()
    return text if text else default

def resolve_executable(generic_name, config_value):
    """Resuelve el ejecutable a usar: PATH primero, luego ruta absoluta configurada.

    Lógica:
    - Si config_value es solo un nombre (sin separadores de ruta), busca en PATH.
    - Si config_value es ruta absoluta y el archivo existe, la usa directamente.
    - Fallback: devuelve config_value tal cual (el error de FileNotFoundError será claro).
    """
    val = str(config_value or "").strip() or generic_name
    is_abs = os.sep in val or (sys.platform == "win32" and len(val) > 1 and val[1] == ":")
    if is_abs:
        if os.path.isfile(val):
            return val, "ruta-absoluta"
        # Ruta absoluta pero no existe; intentar nombre genérico en PATH
        found = shutil.which(generic_name)
        if found:
            return found, "PATH-fallback"
        return val, "no-encontrado"
    # Es un nombre simple; buscar en PATH
    found = shutil.which(val)
    if found:
        return found, "PATH"
    # Último recurso: devolver el valor configurado
    return val, "no-encontrado"

def get_config_credentials():
    cfg = load_config()
    # Try secure storage first; fall back to plaintext in config for backward compat
    if SECURE_TOKEN_AVAILABLE:
        tok, backend = load_token()
        if not tok:
            tok = cfg.get("token", "")
    else:
        tok = cfg.get("token", "")
    client_id = cfg.get("clientId") or cfg.get("client_id", "")
    return tok, client_id

def redacted_config():
    cfg = load_config()
    tok, _ = get_config_credentials()
    safe = {k: cfg[k] for k in SAFE_CONFIG_KEYS if k in cfg}
    safe["hasToken"] = bool(tok)
    safe["hasRefreshToken"] = bool(cfg.get("refresh_token"))
    safe["hasClientId"] = bool(cfg.get("clientId") or cfg.get("client_id"))
    if "client_id" in cfg and "clientId" not in safe:
        safe["clientId"] = cfg["client_id"]
    return safe

def sanitize_config_update(data):
    old = load_config()
    clean = {k: old[k] for k in ("token", "refresh_token") if old.get(k)}
    for key in SAFE_CONFIG_KEYS:
        if key in data:
            clean[key] = data[key]
    for secret_key in ("token", "refresh_token"):
        value = str(data.get(secret_key, "") or "").strip()
        if value:
            if secret_key == "token" and SECURE_TOKEN_AVAILABLE:
                backend = store_token(value)
                if backend != "none":
                    # Stored securely — don't persist in config.json
                    clean.pop("token", None)
                    continue
            clean[secret_key] = value
    clean["quality"] = normalize_quality(clean.get("quality", "best"))
    clean["player"] = normalize_player(clean.get("player", "mpv"))
    clean["mpv"] = normalize_path_value(clean.get("mpv"), "mpv")
    clean["vlc"] = normalize_path_value(clean.get("vlc"), "vlc")
    clean["gp"] = normalize_path_value(clean.get("gp"), "gridplayer")
    clean["sl"] = normalize_path_value(clean.get("sl"), "streamlink")
    clean["ff"] = normalize_path_value(clean.get("ff"), "ffmpeg")
    clean["w"] = clamp_int(clean.get("w"), 1920, 320, 7680)
    clean["h"] = clamp_int(clean.get("h"), 1080, 240, 4320)
    clean["hwaccel"] = bool(clean.get("hwaccel", False))
    clean["hwgpu"] = str(clean.get("hwgpu", "nvidia")).lower()
    if clean["hwgpu"] not in HW_GPUS:
        clean["hwgpu"] = "nvidia"
    clean["vlcFfmpeg"] = bool(clean.get("vlcFfmpeg", False))
    clean["textOnly"] = bool(clean.get("textOnly", False))
    accent = str(clean.get("accentColor", "#e0553a")).strip()
    clean["accentColor"] = accent if re.fullmatch(r"#[0-9a-fA-F]{6}", accent) else "#e0553a"
    return clean

def clear_credentials():
    if SECURE_TOKEN_AVAILABLE:
        delete_token()
    cfg = load_config()
    cfg.pop("token", None)
    cfg.pop("refresh_token", None)
    return save_config(cfg)

def check_command(label, path_value, version_args=None):
    exe = normalize_path_value(path_value, label)
    version_args = version_args or ["--version"]
    result = {"name": label, "path": exe, "ok": False, "detail": ""}
    if os.path.isabs(exe) and not os.path.exists(exe):
        result["detail"] = "No existe en esa ruta"
        return result
    try:
        proc = subprocess.run(
            [exe] + version_args,
            capture_output=True,
            timeout=5,
            creationflags=subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0
        )
        output = (proc.stdout or proc.stderr).decode("utf-8", errors="replace").strip()
        result["ok"] = proc.returncode == 0
        result["detail"] = (output.splitlines()[0] if output else f"exit={proc.returncode}")[:180]
    except FileNotFoundError:
        result["detail"] = "No encontrado"
    except subprocess.TimeoutExpired:
        result["detail"] = "No respondio a tiempo"
    except Exception as e:
        result["detail"] = str(e)[:180]
    return result

def check_gui_executable(label, path_value):
    exe = normalize_path_value(path_value, label)
    result = {"name": label, "path": exe, "ok": False, "detail": ""}
    if os.path.isabs(exe):
        result["ok"] = os.path.exists(exe)
        result["detail"] = "Ruta encontrada" if result["ok"] else "No existe en esa ruta"
        return result
    result["ok"] = True
    result["detail"] = "Ruta no absoluta; se usara el PATH del sistema"
    return result

def dependency_status():
    def _safe_vlc_for_check(value):
        resolved = vlc_gui_path(value)
        if is_vlc_console_wrapper(resolved):
            return ""
        return resolved
    cfg = load_config()
    tok, client_id = get_config_credentials()
    cfg["vlc"] = _safe_vlc_for_check(cfg.get("vlc", "vlc")) or "__vlc_exe_not_found__"

    def _check(generic, cfg_key, version_args=None):
        resolved, method = resolve_executable(generic, cfg.get(cfg_key, generic))
        r = check_command(generic, resolved, version_args)
        r["resolved"] = resolved
        r["method"] = method
        if not r["ok"] and method == "no-encontrado":
            r["install_hint"] = _install_hint(generic)
        return r

    def _check_gui(generic, cfg_key):
        resolved, method = resolve_executable(generic, cfg.get(cfg_key, generic))
        r = check_gui_executable(generic, resolved)
        r["resolved"] = resolved
        r["method"] = method
        if not r["ok"]:
            r["install_hint"] = _install_hint(generic)
        return r

    deps = [
        _check("streamlink", "sl"),
        _check("ffmpeg", "ff", ["-version"]),
    ]
    player = normalize_player(cfg.get("player", "mpv"))
    if player == "vlc":
        deps.append(_check_gui("vlc", "vlc"))
    elif player == "gridplayer":
        deps.append(_check_gui("gridplayer", "gp"))
    else:
        deps.append(_check("mpv", "mpv", ["--version"]))
    return {
        "checker": "resolve-executable",
        "deps": deps,
        "credentials": {
            "hasToken": bool(tok),
            "hasClientId": bool(client_id),
        }
    }

def _install_hint(name):
    hints = {
        "streamlink": "ejecuta install.bat (Windows) o bash install.sh (Linux)",
        "ffmpeg":     "winget install Gyan.FFmpeg  (Windows)  |  sudo apt install ffmpeg  (Linux)",
        "mpv":        "winget install mpv.mpv       (Windows)  |  sudo apt install mpv     (Linux)",
        "vlc":        "winget install VideoLAN.VLC  (Windows)  |  sudo apt install vlc     (Linux)",
        "gridplayer": "https://github.com/vzhd1701/gridplayer/releases",
    }
    return hints.get(name, "")

# ── MINING: LÓGICA ────────────────────────────────────────────

def _resolve_channel_id(channel_id):
    """Resuelve channel_id → channel_login via Helix. Cachea el resultado."""
    global _channel_id_cache
    if not channel_id:
        return ""
    cached = _channel_id_cache.get(str(channel_id))
    if cached:
        return cached
    token, client_id = get_config_credentials()
    if not token or not client_id:
        return f"__id_{channel_id}"
    try:
        result = get_user_info_by_id([str(channel_id)], token, client_id)
        login  = result.get(str(channel_id), "")
        if login:
            _channel_id_cache[str(channel_id)] = login
            # Renombrar key en points_state si existe como __id_X
            old_key = f"__id_{channel_id}"
            with mining_lock:
                if old_key in points_state:
                    old = points_state.pop(old_key)
                    cur = points_state.setdefault(login, {
                        "balance": 0, "claim_count": 0,
                        "session_earned": 0, "last_claim_ts": 0
                    })
                    cur["balance"] = max(int(cur.get("balance", 0) or 0), int(old.get("balance", 0) or 0))
                    cur["claim_count"] = int(cur.get("claim_count", 0) or 0) + int(old.get("claim_count", 0) or 0)
                    cur["session_earned"] = int(cur.get("session_earned", 0) or 0) + int(old.get("session_earned", 0) or 0)
                    cur["last_claim_ts"] = max(float(cur.get("last_claim_ts", 0) or 0), float(old.get("last_claim_ts", 0) or 0))
                    cur["last_earned_ts"] = max(float(cur.get("last_earned_ts", 0) or 0), float(old.get("last_earned_ts", 0) or 0))
                    cur["last_gain"] = old.get("last_gain", cur.get("last_gain", 0))
                    cur["last_reason"] = old.get("last_reason", cur.get("last_reason", ""))
                    cur["channel_id"] = str(channel_id)
        return login or f"__id_{channel_id}"
    except Exception:
        return f"__id_{channel_id}"

def get_user_info_by_id(channel_ids, token, client_id):
    """Obtiene login por lista de channel IDs via Helix /helix/users?id=X."""
    if not channel_ids:
        return {}
    query = "&".join(f"id={cid}" for cid in channel_ids)
    url   = f"https://api.twitch.tv/helix/users?{query}"
    try:
        req = urllib.request.Request(url, headers={
            "Authorization": f"Bearer {token}",
            "Client-Id":     client_id,
        })
        with urllib.request.urlopen(req, timeout=5) as r:
            data = json.loads(r.read())
        return {u["id"]: u["login"].lower() for u in data.get("data", [])}
    except Exception:
        return {}

def _add_mining_log(event_type, channel, detail, points=None):
    """Agrega un evento al log de mining (máx 100 entradas)."""
    if not mining_lock.acquire(timeout=1.0):
        print(f"  [Mining] log omitido por lock ocupado: {event_type} {channel} {detail}")
        return
    try:
        entry = {
            "ts":      time.time(),
            "type":    event_type,
            "channel": channel,
            "detail":  detail,
        }
        if points is not None:
            entry["points"] = points
        mining_log.append(entry)
        if len(mining_log) > 100:
            mining_log.pop(0)
    finally:
        mining_lock.release()

def on_pubsub_event(event_type, data):
    """Callback del PubSubManager. Se llama desde el hilo WebSocket."""
    token, _ = get_config_credentials()

    if event_type == "status":
        connected = data.get("connected", False)
        reason    = data.get("reason", "")
        print(f"  [Mining] PubSub {'conectado' if connected else 'desconectado'} ({reason})")
        _add_mining_log("status", "", "conectado" if connected else f"desconectado: {reason}")
        return

    if event_type == "claim-available":
        channel_id    = data.get("channel_id", "")
        raw_login     = data.get("channel_login", "")
        # Si pubsub no tenía login real, intentar desde cache o resolver via Helix
        if not raw_login or raw_login.startswith("__id_"):
            channel_login = _channel_id_cache.get(str(channel_id)) or _resolve_channel_id(channel_id)
        else:
            channel_login = raw_login
        claim_id      = data.get("claim_id", "")
        if not (channel_id and claim_id and GQL_AVAILABLE and token):
            return
        def _do_claim():
            new_balance = gql_claim_points(channel_id, claim_id, token)
            if new_balance is not None:
                with mining_lock:
                    ch = points_state.setdefault(channel_login, {
                        "balance": 0, "claim_count": 0,
                        "session_earned": 0, "last_claim_ts": 0
                    })
                    if channel_id:
                        ch["channel_id"] = str(channel_id)
                    ch["claim_count"]  += 1
                    ch["last_claim_ts"] = time.time()
                    if new_balance > 0:
                        earned = max(new_balance - ch.get("balance", 0), 0)
                        ch["balance"]        = new_balance
                        ch["session_earned"] = ch.get("session_earned", 0) + earned
                        ch["last_gain"]      = earned
                        ch["last_reason"]    = "CLAIM"
                        ch["last_earned_ts"] = time.time()
                        print(f"  [Mining] ✓ Claim en {channel_login}: +{earned} → {new_balance}")
                        _add_mining_log("points-claimed", channel_login, f"+{earned} pts (chest)", new_balance)
                    else:
                        # -1 = ya fue reclamado (por el navegador); los puntos
                        # llegarán via evento points-earned
                        print(f"  [Mining] ✓ Claim enviado en {channel_login} (puntos via PubSub)")
                        _add_mining_log("points-claimed", channel_login, "chest (pts via PubSub)")
            else:
                gql_error = (gql_get_last_error() or "").strip()
                if "integrity" in gql_error.lower():
                    detail = "Twitch exige verificacion de integridad; reclama este cofre manualmente en el navegador"
                else:
                    detail = f"fallo en GQL: {gql_error[:160]}" if gql_error else "fallo en GQL"
                print(f"  [Mining] Fallo al reclamar puntos en {channel_login}: {detail}")
                _add_mining_log("points-claim-fail", channel_login, detail)
        threading.Thread(target=_do_claim, daemon=True).start()

    elif event_type == "points-earned":
        channel_login = data.get("channel_login", "")
        channel_id    = data.get("channel_id", "")
        balance       = data.get("balance", 0)
        earned        = data.get("points_earned", 0)
        reason        = data.get("reason_code", "")
        # channel_login puede venir como __id_X desde pubsub.py — intentar resolver
        is_placeholder = channel_login.startswith("__id_") if channel_login else True
        if is_placeholder and channel_id:
            # Usar cache si ya está resuelto
            cached_login = _channel_id_cache.get(str(channel_id))
            if cached_login:
                channel_login = cached_login
                is_placeholder = False
            else:
                # Resolver en hilo (renombra key en points_state al terminar)
                threading.Thread(
                    target=_resolve_channel_id, args=(channel_id,), daemon=True
                ).start()
        key = channel_login if not is_placeholder else (_channel_id_cache.get(str(channel_id)) or f"__id_{channel_id}")
        print(f"  [Mining] pts-earned: {key} +{earned} → {balance} ({reason})")
        with mining_lock:
            ch = points_state.setdefault(key, {
                "balance": 0, "claim_count": 0,
                "session_earned": 0, "last_claim_ts": 0
            })
            ch["balance"]        = balance
            ch["session_earned"] = ch.get("session_earned", 0) + max(earned, 0)
            ch["last_gain"]      = max(earned, 0)
            ch["last_reason"]    = reason or "UNKNOWN"
            ch["last_earned_ts"] = time.time()
            if channel_id:
                ch["channel_id"] = str(channel_id)
        _add_mining_log("points-earned", key, f"+{earned} ({reason or 'UNKNOWN'})", balance)

    elif event_type == "moment-available":
        # Momentos de clip (puntos extra)
        moment_id  = data.get("moment_id", "")
        channel_id = data.get("channel_id", "")
        if not (moment_id and channel_id and GQL_AVAILABLE and token):
            return
        def _do_moment():
            # ClaimMoment requiere GQL separado; loggeamos por ahora
            _add_mining_log("moment-available", channel_id, f"moment_id={moment_id}")
        threading.Thread(target=_do_moment, daemon=True).start()

    elif event_type == "drop-progress":
        drop_id  = data.get("drop_id", "")
        if not drop_id:
            return
        with mining_lock:
            entry = drops_state.setdefault(drop_id, {
                "name": "", "campaign": "", "game": "", "claimed": False
            })
            entry.update({
                "current_minutes":  data.get("current_minutes", 0),
                "required_minutes": data.get("required_minutes", 0),
                "percent":          data.get("percent", 0),
                "channel_login":    data.get("channel_login", ""),
            })

    elif event_type == "drop-claim":
        drop_instance_id = data.get("drop_instance_id", "")
        channel_login    = data.get("channel_login", "")
        if not (drop_instance_id and GQL_AVAILABLE and token):
            return
        def _do_drop_claim():
            ok = gql_claim_drop(drop_instance_id, token)
            if ok:
                with mining_lock:
                    for d in drops_state.values():
                        if d.get("channel_login") == channel_login:
                            d["claimed"] = True
                print(f"  [Mining] ✓ Drop reclamado en {channel_login} (id={drop_instance_id[:8]}...)")
                _add_mining_log("drop-claimed", channel_login, f"drop id={drop_instance_id[:8]}")
            else:
                print(f"  [Mining] ✗ Fallo al reclamar drop en {channel_login}")
                _add_mining_log("drop-claim-fail", channel_login, "fallo en GQL")
        threading.Thread(target=_do_drop_claim, daemon=True).start()


def _resolve_pending_channel_ids():
    """Resuelve todos los __id_X pendientes en points_state via Helix."""
    token, client_id = get_config_credentials()
    if not token or not client_id:
        return
    with mining_lock:
        pending = [k.replace("__id_", "") for k in points_state if k.startswith("__id_")]
    if not pending:
        return
    result = get_user_info_by_id(pending, token, client_id)
    for ch_id, login in result.items():
        if not login:
            continue
        _channel_id_cache[ch_id] = login
        old_key = f"__id_{ch_id}"
        with mining_lock:
            if old_key in points_state:
                old = points_state.pop(old_key)
                cur = points_state.setdefault(login, {
                    "balance": 0, "claim_count": 0,
                    "session_earned": 0, "last_claim_ts": 0
                })
                cur["balance"] = max(int(cur.get("balance", 0) or 0), int(old.get("balance", 0) or 0))
                cur["claim_count"] = int(cur.get("claim_count", 0) or 0) + int(old.get("claim_count", 0) or 0)
                cur["session_earned"] = int(cur.get("session_earned", 0) or 0) + int(old.get("session_earned", 0) or 0)
                cur["last_claim_ts"] = max(float(cur.get("last_claim_ts", 0) or 0), float(old.get("last_claim_ts", 0) or 0))
                cur["last_earned_ts"] = max(float(cur.get("last_earned_ts", 0) or 0), float(old.get("last_earned_ts", 0) or 0))
                cur["last_gain"] = old.get("last_gain", cur.get("last_gain", 0))
                cur["last_reason"] = old.get("last_reason", cur.get("last_reason", ""))
                cur["channel_id"] = str(ch_id)
                print(f"  [Mining] Resuelto: {old_key} → {login}")


def _refresh_points_balances():
    """Refresca el balance de puntos de todos los canales activos. Se llama cada 5 min."""
    global _balance_timer
    token, client_id = get_config_credentials()
    if not token:
        return
    # Resolver IDs pendientes primero
    _resolve_pending_channel_ids()
    # Pre-poblar IDs para canales activos que aún no estén en el cache
    with lock:
        active = list(procesos.keys())
    if active and client_id:
        # Obtener channel_ids de los canales activos para pre-poblar el cache
        try:
            info = get_user_info(active, token, client_id)
            # get_user_info devuelve {login: {display_name, avatar}} — necesitamos el id inverso
            # Usar Helix directamente para obtener id→login
            q   = "&".join(f"login={c}" for c in active)
            req = urllib.request.Request(
                f"https://api.twitch.tv/helix/users?{q}",
                headers={"Authorization": f"Bearer {token}", "Client-Id": client_id}
            )
            with urllib.request.urlopen(req, timeout=5) as r:
                hdata = json.loads(r.read())
            for u in hdata.get("data", []):
                _channel_id_cache[u["id"]] = u["login"].lower()
        except Exception:
            pass
    if not GQL_AVAILABLE:
        return
    for channel in active:
        try:
            bal = gql_get_balance(channel, token)
            if bal is not None:
                with mining_lock:
                    ch = points_state.setdefault(channel, {
                        "balance": 0, "claim_count": 0,
                        "session_earned": 0, "last_claim_ts": 0
                    })
                    ch["balance"] = bal
        except Exception as e:
            print(f"  [Mining] Error refrescando balance {channel}: {e}")
    # También refrescar drops desde inventario
    try:
        inv_drops = gql_get_inventory(token)
        with mining_lock:
            for d in inv_drops:
                drop_id = d.get("drop_id", "")
                if drop_id:
                    existing = drops_state.get(drop_id, {})
                    existing.update(d)
                    drops_state[drop_id] = existing
    except Exception as e:
        print(f"  [Mining] Error refrescando inventario drops: {e}")
    # Re-programar
    cfg = load_config()
    if cfg.get("miningEnabled") and pubsub_mgr and pubsub_mgr.running:
        _balance_timer = threading.Timer(300, _refresh_points_balances)
        _balance_timer.daemon = True
        _balance_timer.start()


def start_mining():
    """Inicia el PubSubManager. Obtiene user_id si no lo tiene."""
    global pubsub_mgr, _balance_timer
    if not PubSubManager or not WEBSOCKET_AVAILABLE:
        return False, "websocket-client no instalado. Ejecuta install.bat o bash install.sh"
    token, _ = get_config_credentials()
    if not token:
        return False, "No hay token configurado"
    if pubsub_mgr and pubsub_mgr.running:
        return False, "Mining ya activo"
    # Obtener user_id
    user = get_my_user(token, load_config().get("clientId", ""))
    if not user:
        return False, "No se pudo obtener user_id (token inválido?)"
    user_id = user["id"]
    try:
        pubsub_mgr = PubSubManager(token, user_id, on_pubsub_event)
    except RuntimeError as e:
        return False, str(e)
    pubsub_mgr.start()
    # Cancel any existing timer before creating new one (race condition fix)
    if _balance_timer:
        _balance_timer.cancel()
        _balance_timer = None
    _balance_timer = threading.Timer(10, _refresh_points_balances)
    _balance_timer.daemon = True
    _balance_timer.start()
    print(f"  [Mining] Iniciado para user_id={user_id}")
    _add_mining_log("mining-start", "", f"user_id={user_id}")
    return True, f"Mining iniciado (user_id={user_id})"


def stop_mining():
    """Detiene el PubSubManager."""
    global pubsub_mgr, _balance_timer
    if _balance_timer:
        _balance_timer.cancel()
        _balance_timer = None
    if pubsub_mgr and pubsub_mgr.running:
        pubsub_mgr.stop()
        _add_mining_log("mining-stop", "", "detenido por usuario")
        return True, "Mining detenido"
    return False, "Mining no estaba activo"


def mining_status():
    """Devuelve estado completo del mining para /api/mining/status."""
    global pubsub_mgr
    running   = bool(pubsub_mgr and pubsub_mgr.running)
    connected = bool(pubsub_mgr and pubsub_mgr.connected)
    got_lock = mining_lock.acquire(timeout=0.5)
    try:
        if got_lock:
            ps   = {k: dict(v) for k, v in points_state.items()}
            ds   = dict(drops_state)
            log  = list(mining_log[-50:])
        else:
            ps, ds, log = {}, {}, [{
                "ts": time.time(),
                "type": "status",
                "channel": "",
                "detail": "estado ocupado; reintentando"
            }]
    finally:
        if got_lock:
            mining_lock.release()
    for ch, data in ps.items():
        data.setdefault("balance", 0)
        data.setdefault("session_earned", 0)
        data.setdefault("claim_count", 0)
        data.setdefault("last_gain", 0)
        data.setdefault("last_reason", "")
        data.setdefault("last_earned_ts", 0)
        data.setdefault("channel", ch)
    return {
        "available":   bool(PubSubManager) and bool(GQL_AVAILABLE),
        "running":     running,
        "connected":   connected,
        "points":      ps,
        "drops":       list(ds.values()),
        "log":         log,
        "websocket_ok": WEBSOCKET_AVAILABLE,
        "gql_ok":      GQL_AVAILABLE,
        "watch":       _watch_status_snapshot(),
        "busy":        not got_lock,
    }


# ── WATCH HEARTBEAT: LÓGICA ───────────────────────────────────

WATCH_INTERVAL = 60   # segundos entre heartbeats

def _watch_status_snapshot():
    got_lock = watch_lock.acquire(timeout=0.5)
    if not got_lock:
        return {}
    try:
        return {
            c: {
                "enabled":    w.get("enabled", False),
                "ticks":      w.get("ticks", 0),
                "last_ts":    w.get("last_ts", 0),
                "game_name":  w.get("game_name", ""),
                "last_error": w.get("last_error", ""),
                "fail_count": w.get("fail_count", 0),
            }
            for c, w in watch_state.items()
        }
    finally:
        watch_lock.release()

def _do_watch_tick_legacy_unused(canal):
    """Envía un heartbeat minute-watched y reprograma el siguiente tick."""
    if not GQL_AVAILABLE:
        return
    with watch_lock:
        entry = watch_state.get(canal)
        if not entry or not entry.get("enabled"):
            return
        token   = entry.get("token", "")
        uid     = entry.get("user_id", "")
        ch_id   = entry.get("channel_id", "")
        bc_id   = entry.get("broadcast_id", "")
        game_id = entry.get("game_id", "")

    if not (token and uid and ch_id and bc_id):
        return

    ok = gql_send_watch(ch_id, bc_id, uid, game_id, token)
    with watch_lock:
        if canal not in watch_state:
            return
        if ok:
            watch_state[canal]["ticks"]   = watch_state[canal].get("ticks", 0) + 1
            watch_state[canal]["last_ts"] = time.time()
        # Reprogramar si sigue habilitado
        if watch_state[canal].get("enabled"):
            t = threading.Timer(WATCH_INTERVAL, _do_watch_tick, args=(canal,))
            t.daemon = True
            watch_state[canal]["timer"] = t
            t.start()


def _do_watch_tick(canal):
    """Robust heartbeat: no deja que un fallo de red mate el timer."""
    if not GQL_AVAILABLE:
        _add_mining_log("watch-fail", canal, "GQL no disponible")
        return
    with watch_lock:
        entry = watch_state.get(canal)
        if not entry or not entry.get("enabled"):
            return
        token   = entry.get("token", "")
        uid     = entry.get("user_id", "")
        ch_id   = entry.get("channel_id", "")
        bc_id   = entry.get("broadcast_id", "")
        game_id = entry.get("game_id", "")

    if not (token and uid and ch_id and bc_id):
        with watch_lock:
            if canal in watch_state:
                watch_state[canal]["last_error"] = "faltan datos watch"
                watch_state[canal]["fail_count"] = watch_state[canal].get("fail_count", 0) + 1
        _add_mining_log("watch-fail", canal, "faltan datos watch")
        return

    ok = False
    err = ""
    try:
        ok = bool(gql_send_watch(ch_id, bc_id, uid, game_id, token))
    except Exception as e:
        err = str(e)[:160]
    with watch_lock:
        if canal not in watch_state:
            return
        if ok:
            watch_state[canal]["ticks"] = watch_state[canal].get("ticks", 0) + 1
            watch_state[canal]["last_ts"] = time.time()
            watch_state[canal]["last_error"] = ""
        else:
            watch_state[canal]["last_error"] = err or "heartbeat rechazado"
            watch_state[canal]["fail_count"] = watch_state[canal].get("fail_count", 0) + 1
        if watch_state[canal].get("enabled"):
            t = threading.Timer(WATCH_INTERVAL, _do_watch_tick, args=(canal,))
            t.daemon = True
            watch_state[canal]["timer"] = t
            t.start()
    _add_mining_log("watch-tick" if ok else "watch-fail", canal, "minute-watched ok" if ok else (err or "heartbeat rechazado"))

def start_channel_watch(canal):
    """Inicia el heartbeat minute-watched para un canal activo."""
    if not GQL_AVAILABLE:
        return False, "GQL no disponible"
    token, _ = get_config_credentials()
    if not token:
        return False, "Sin token"
    # Verificar canal activo
    with lock:
        if canal not in procesos:
            return False, "Canal no está reproduciéndose"

    # Obtener user_id (del mining si ya arrancó, si no de la API)
    user_id = ""
    with mining_lock:
        for ch_data in points_state.values():
            break   # solo necesitamos el user_id global
    if pubsub_mgr:
        user_id = getattr(pubsub_mgr, "_user_id", "")
    if not user_id:
        cfg   = load_config()
        user  = get_my_user(token, cfg.get("clientId", ""))
        if user:
            user_id = str(user["id"])
    if not user_id:
        return False, "No se pudo obtener user_id"

    # Obtener info del stream en curso
    info = gql_get_watch_info(canal, token)
    if not info:
        return False, f"{canal} no parece estar en vivo (GQL)"

    with watch_lock:
        # Cancelar timer anterior si existe
        old = watch_state.get(canal, {})
        if old.get("timer"):
            try: old["timer"].cancel()
            except Exception: pass
        watch_state[canal] = {
            "enabled":    True,
            "channel_id": info["channel_id"],
            "broadcast_id": info["broadcast_id"],
            "game_id":    info["game_id"],
            "game_name":  info["game_name"],
            "user_id":    user_id,
            "token":      token,
            "ticks":      0,
            "last_ts":    0,
            "last_error": "",
            "fail_count": 0,
            "timer":      None,
        }

    # Primer tick inmediato en hilo separado
    threading.Thread(target=_do_watch_tick, args=(canal,), daemon=True).start()
    _add_mining_log("watch-start", canal, f"game={info['game_name']}")
    print(f"  [Watch] ▶ {canal} ({info['game_name']})")
    return True, "Watch iniciado"


def stop_channel_watch(canal, forget=False):
    """Detiene el heartbeat minute-watched para un canal."""
    ticks = 0
    with watch_lock:
        entry = watch_state.get(canal)
        if not entry:
            return True, "Watch no estaba activo"
        ticks = entry.get("ticks", 0)
        entry["enabled"] = False
        t = entry.get("timer")
        if t:
            try: t.cancel()
            except Exception: pass
        entry["timer"] = None
        if forget:
            watch_state.pop(canal, None)
    _add_mining_log("watch-stop", canal, f"ticks={ticks}")
    print(f"  [Watch] ⏹ {canal}")
    return True, "Watch detenido"


def _stop_all_watches():
    """Detiene todos los watches (llamado por cerrar_todos)."""
    with watch_lock:
        canales = list(watch_state.keys())
    for c in canales:
        stop_channel_watch(c)


def read_log_tail(name, lines=80):
    safe_name = "grid" if name == "grid" else normalize_channel(name)
    if not safe_name:
        return None
    path = _grid_log_path() if safe_name == "grid" else _channel_log_path(safe_name)
    real = os.path.realpath(path)
    if os.path.commonpath([LOG_STREAMS_DIR, real]) != LOG_STREAMS_DIR or not os.path.exists(real):
        return ""
    try:
        with open(real, "r", encoding="utf-8", errors="replace") as f:
            return "".join(f.readlines()[-lines:])
    except OSError:
        return ""

def _ensure_log_dirs():
    os.makedirs(LOG_STREAMS_DIR, exist_ok=True)
    os.makedirs(LOG_APP_DIR, exist_ok=True)
    os.makedirs(RUNTIME_APP_DIR, exist_ok=True)

def _move_or_append_text_file(old_path, new_path):
    if not os.path.exists(old_path):
        return
    try:
        os.makedirs(os.path.dirname(new_path), exist_ok=True)
        if os.path.exists(new_path):
            with open(old_path, "r", encoding="utf-8", errors="replace") as src, \
                 open(new_path, "a", encoding="utf-8", errors="replace") as dst:
                dst.write(src.read())
            os.remove(old_path)
        else:
            os.replace(old_path, new_path)
    except OSError:
        pass

def _move_or_replace_file(old_path, new_path):
    if not os.path.exists(old_path):
        return
    try:
        os.makedirs(os.path.dirname(new_path), exist_ok=True)
        if os.path.exists(new_path):
            os.remove(old_path)
        else:
            os.replace(old_path, new_path)
    except OSError:
        pass

def _migrate_legacy_runtime_files():
    _ensure_log_dirs()
    _move_or_append_text_file(os.path.join(BASE, "process-launch.log"), _app_log_path("process-launch.log"))
    _move_or_append_text_file(os.path.join(BASE, "vlc-help.txt"), _app_log_path("vlc-help.txt"))
    _move_or_replace_file(os.path.join(BASE, "url_cache.json"), URL_CACHE_FILE)
    _move_or_replace_file(os.path.join(BASE, "mosaic-vlm.conf"), VLM_CONF)
    _move_or_replace_file(os.path.join(BASE, "vlc_bg.ts"), _runtime_app_path("vlc_bg.ts"))

def _channel_log_path(canal):
    safe_name = normalize_channel(canal)
    if not safe_name:
        return None
    return os.path.join(LOG_STREAMS_DIR, f"{safe_name}.log")

def _grid_log_path():
    return os.path.join(LOG_STREAMS_DIR, "grid.log")

def _app_log_path(filename):
    return os.path.join(LOG_APP_DIR, filename)

def _runtime_app_path(filename):
    return os.path.join(RUNTIME_APP_DIR, filename)

def _is_channel_log_name(filename):
    stem, ext = os.path.splitext(filename)
    return ext.lower() == ".log" and bool(CHANNEL_RE.fullmatch(stem))

def load_url_cache():
    try:
        with open(URL_CACHE_FILE, "r", encoding="utf-8") as f:
            return json.load(f)
    except Exception:
        return {}

def save_url_cache(cache):
    try:
        _ensure_log_dirs()
        with open(URL_CACHE_FILE, "w", encoding="utf-8") as f:
            json.dump(cache, f, indent=2)
    except Exception as e:
        print(f"  Error guardando url_cache: {e}")

def verify_hls_url(url):
    """HEAD request para verificar que la URL HLS sigue activa."""
    try:
        req = urllib.request.Request(url, method="HEAD")
        with urllib.request.urlopen(req, timeout=4) as r:
            return r.status == 200
    except Exception:
        return False

_LOG_REDACT_RE = re.compile(
    r'(Authorization=|Bearer |oauth:)([A-Za-z0-9_\-]{8,})',
    re.IGNORECASE
)

def sanitize_for_log(text):
    """Redact OAuth tokens and bearer tokens from log strings."""
    return _LOG_REDACT_RE.sub(lambda m: m.group(1) + "***REDACTED***", str(text or ""))


def load_config():
    try:
        with open(CONFIG_FILE, "r", encoding="utf-8") as f:
            return json.load(f)
    except Exception:
        return {}

def save_config(data):
    try:
        # Backup before overwrite
        bak = CONFIG_FILE + ".bak"
        if os.path.exists(CONFIG_FILE):
            try:
                shutil.copy2(CONFIG_FILE, bak)
            except Exception:
                pass
        with open(CONFIG_FILE, "w", encoding="utf-8") as f:
            json.dump(data, f, indent=2, ensure_ascii=False)
        # Restrict permissions on Windows via icacls
        if sys.platform == "win32":
            try:
                username = os.environ.get("USERNAME", "")
                if username:
                    subprocess.run(
                        ["icacls", CONFIG_FILE, "/inheritance:r",
                         "/grant:r", f"{username}:(R,W)"],
                        capture_output=True,
                        creationflags=subprocess.CREATE_NO_WINDOW
                    )
            except Exception:
                pass
        else:
            os.chmod(CONFIG_FILE, 0o600)
        return True
    except Exception as e:
        print(f"  Error guardando config: {e}")
        return False

# ── TWITCH API ────────────────────────────────────────────────

def twitch_headers(token, client_id):
    return {
        "Authorization": f"Bearer {token}",
        "Client-Id":     client_id
    }

def get_streams_info(canales, token, client_id):
    if not token or not client_id or not canales:
        return {}
    key = ("live", tuple(sorted(canales)), client_id, hash(token))
    return cached_twitch_api(key, TWITCH_API_CACHE_TTL["live"], lambda: _get_streams_info_uncached(canales, token, client_id))

def _get_streams_info_uncached(canales, token, client_id):
    """Consulta la API de Twitch para obtener estado, viewers y título de varios canales"""
    if not token or not client_id or not canales:
        return {}

    query = "&".join(f"user_login={c}" for c in canales)
    url   = f"https://api.twitch.tv/helix/streams?{query}&first=100"

    try:
        req = urllib.request.Request(url, headers=twitch_headers(token, client_id))
        with urllib.request.urlopen(req, timeout=5) as r:
            data = json.loads(r.read())
        result = {}
        for s in data.get("data", []):
            result[s["user_login"].lower()] = {
                "live":         True,
                "title":        s.get("title", ""),
                "viewers":      s.get("viewer_count", 0),
                "game":         s.get("game_name", ""),
                "thumbnail":    s.get("thumbnail_url", "").replace("{width}", "320").replace("{height}", "180"),
                "started_at":   s.get("started_at", "")
            }
        return result
    except Exception as e:
        print(f"  Error API Twitch: {e}")
        return {}

def get_my_user(token, client_id):
    if not token or not client_id:
        return None
    key = ("me", client_id, hash(token))
    return cached_twitch_api(key, TWITCH_API_CACHE_TTL["users"], lambda: _get_my_user_uncached(token, client_id))

def _get_my_user_uncached(token, client_id):
    try:
        req = urllib.request.Request("https://api.twitch.tv/helix/users", headers=twitch_headers(token, client_id))
        with urllib.request.urlopen(req, timeout=5) as r:
            data = json.loads(r.read())
        users = data.get("data", [])
        return users[0] if users else None
    except:
        return None

def get_followed_live(token, client_id):
    if not token or not client_id:
        return []
    key = ("followed", client_id, hash(token))
    return cached_twitch_api(key, TWITCH_API_CACHE_TTL["followed"], lambda: _get_followed_live_uncached(token, client_id))

def _get_followed_live_uncached(token, client_id):
    user = get_my_user(token, client_id)
    if not user:
        return []
    try:
        url = f"https://api.twitch.tv/helix/streams/followed?user_id={user['id']}&first=100"
        req = urllib.request.Request(url, headers=twitch_headers(token, client_id))
        with urllib.request.urlopen(req, timeout=8) as r:
            data = json.loads(r.read())
        return [{
            "login":        s["user_login"].lower(),
            "display_name": s.get("user_name", s["user_login"]),
            "title":        s.get("title", ""),
            "viewers":      s.get("viewer_count", 0),
            "game":         s.get("game_name", ""),
            "thumbnail":    s.get("thumbnail_url", "").replace("{width}","320").replace("{height}","180"),
            "started_at":   s.get("started_at", "")
        } for s in data.get("data", [])]
    except Exception as e:
        print(f"  Error followed: {e}")
        return []

def get_top_categories(token, client_id, cursor=""):
    if not token or not client_id:
        return [], ""
    key = ("categories", cursor, client_id, hash(token))
    return cached_twitch_api(key, TWITCH_API_CACHE_TTL["categories"], lambda: _get_top_categories_uncached(token, client_id, cursor))

def _get_top_categories_uncached(token, client_id, cursor=""):
    if not token or not client_id:
        return [], ""
    try:
        url = f"https://api.twitch.tv/helix/games/top?first=24" + (f"&after={cursor}" if cursor else "")
        req = urllib.request.Request(url, headers=twitch_headers(token, client_id))
        with urllib.request.urlopen(req, timeout=5) as r:
            data = json.loads(r.read())
        cats = [{"id": g["id"], "name": g["name"],
                 "box_art": g.get("box_art_url","").replace("{width}","130").replace("{height}","180")}
                for g in data.get("data", [])]
        return cats, data.get("pagination",{}).get("cursor","")
    except Exception as e:
        print(f"  Error categories: {e}")
        return [], ""

def get_recommended(token, client_id):
    if not token or not client_id:
        return []
    key = ("recommended", client_id, hash(token))
    return cached_twitch_api(key, TWITCH_API_CACHE_TTL["recommended"], lambda: _get_recommended_uncached(token, client_id))

def _get_recommended_uncached(token, client_id):
    if not token or not client_id:
        return []
    try:
        url = "https://api.twitch.tv/helix/streams?first=20"
        req = urllib.request.Request(url, headers=twitch_headers(token, client_id))
        with urllib.request.urlopen(req, timeout=5) as r:
            data = json.loads(r.read())
        return [{"user_name": s["user_name"], "title": s["title"],
                 "game_name": s.get("game_name",""), "viewer_count": s["viewer_count"],
                 "thumbnail": s.get("thumbnail_url","").replace("{width}","320").replace("{height}","180")}
                for s in data.get("data", [])]
    except Exception as e:
        print(f"  Error recommended: {e}")
        return []

def search_categories(query, token, client_id):
    query = str(query or "").strip()
    if not token or not client_id or not query:
        return []
    key = ("search", query.lower(), client_id, hash(token))
    return cached_twitch_api(key, TWITCH_API_CACHE_TTL["search"], lambda: _search_categories_uncached(query, token, client_id))

def _search_categories_uncached(query, token, client_id):
    if not token or not client_id or not query:
        return []
    try:
        url = f"https://api.twitch.tv/helix/search/categories?first=20&query={urllib.parse.quote(query)}"
        req = urllib.request.Request(url, headers=twitch_headers(token, client_id))
        with urllib.request.urlopen(req, timeout=5) as r:
            data = json.loads(r.read())
        return [{"id": g["id"], "name": g["name"],
                 "box_art": g.get("box_art_url","").replace("{width}","130").replace("{height}","180")}
                for g in data.get("data", [])]
    except Exception as e:
        print(f"  Error search_cats: {e}")
        return []

def get_category_streams(game_id, token, client_id, cursor=""):
    if not token or not client_id or not game_id:
        return [], ""
    key = ("cat_streams", str(game_id), cursor, client_id, hash(token))
    return cached_twitch_api(key, TWITCH_API_CACHE_TTL["cat_streams"], lambda: _get_category_streams_uncached(game_id, token, client_id, cursor))

def _get_category_streams_uncached(game_id, token, client_id, cursor=""):
    if not token or not client_id or not game_id:
        return [], ""
    try:
        url = f"https://api.twitch.tv/helix/streams?game_id={game_id}&first=20" + (f"&after={cursor}" if cursor else "")
        req = urllib.request.Request(url, headers=twitch_headers(token, client_id))
        with urllib.request.urlopen(req, timeout=5) as r:
            data = json.loads(r.read())
        return [{
            "login":        s["user_login"].lower(),
            "display_name": s.get("user_name", s["user_login"]),
            "title":        s.get("title", ""),
            "viewers":      s.get("viewer_count", 0),
            "game":         s.get("game_name", ""),
            "thumbnail":    s.get("thumbnail_url","").replace("{width}","320").replace("{height}","180"),
        } for s in data.get("data", [])], data.get("pagination",{}).get("cursor","")
    except Exception as e:
        print(f"  Error cat streams: {e}")
        return [], ""

def get_user_info(canales, token, client_id):
    if not token or not client_id or not canales:
        return {}
    key = ("users", tuple(sorted(canales)), client_id, hash(token))
    return cached_twitch_api(key, TWITCH_API_CACHE_TTL["users"], lambda: _get_user_info_uncached(canales, token, client_id))

def _get_user_info_uncached(canales, token, client_id):
    """Obtiene avatar y display name de usuarios"""
    if not token or not client_id or not canales:
        return {}
    query = "&".join(f"login={c}" for c in canales)
    url   = f"https://api.twitch.tv/helix/users?{query}"
    try:
        req = urllib.request.Request(url, headers=twitch_headers(token, client_id))
        with urllib.request.urlopen(req, timeout=5) as r:
            data = json.loads(r.read())
        return {
            u["login"].lower(): {
                "display_name": u.get("display_name", u["login"]),
                "avatar":       u.get("profile_image_url", "")
            }
            for u in data.get("data", [])
        }
    except:
        return {}

# ── STREAMLINK / MPV ─────────────────────────────────────────

def get_hls_url(canal, calidad, token, sl_path):
    # Clave de cache: "canal:audio" para audio_only, "canal" para video.
    # Evita que URLs de audio contaminen plays normales y viceversa.
    is_audio  = calidad == "audio_only"
    cache_key = f"{canal}:audio" if is_audio else canal

    cache = load_url_cache()
    entry = cache.get(cache_key)
    if entry:
        age = time.time() - entry.get("ts", 0)
        if age < URL_CACHE_TTL:
            print(f"  Cache hit [{cache_key}] ({int(age/60)}min) — verificando...")
            if verify_hls_url(entry["url"]):
                print(f"  Cache válida → usando sin streamlink")
                return entry["url"], entry["quality"]
            else:
                print(f"  Cache expirada/inválida → streamlink")

    # Para audio_only intentamos ese quality primero; si falla bajamos a worst
    fallback = ["audio_only", "worst"] if is_audio else [calidad, "720p60", "720p", "480p", "360p", "worst"]

    tried = []
    for q in fallback:
        if q in tried:
            continue
        tried.append(q)
        try:
            a = [sl_path, "--stream-url"]
            if token:
                a += ["--twitch-api-header", f"Authorization={token}"]
            a += [f"twitch.tv/{canal}", q]
            if q == tried[0]:
                print(f"  CMD: {sanitize_for_log(' '.join(a))}")
            result = subprocess.run(
                a, capture_output=True, timeout=15,
                creationflags=subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0
            )
            url = result.stdout.decode("utf-8", errors="replace").strip()
            if url.startswith("http"):
                cache = load_url_cache()
                cache[cache_key] = {"url": url, "quality": q, "ts": time.time()}
                save_url_cache(cache)
                return url, q
            err = result.stderr.decode("utf-8", errors="replace").strip()
            if err:
                print(f"  streamlink [{canal}@{q}] stderr: {err[:300]}")
            else:
                print(f"  streamlink [{canal}@{q}] sin salida (exit={result.returncode})")
        except FileNotFoundError:
            print(f"  ERROR: streamlink no encontrado en '{sl_path}'")
            return None, None
        except subprocess.TimeoutExpired:
            print(f"  streamlink [{canal}@{q}] timeout")
            continue
        except Exception as e:
            print(f"  streamlink [{canal}@{q}] excepción: {e}")
            continue
    return None, None

def _proc_from_meta(meta):
    if isinstance(meta, dict):
        return meta.get("proc")
    return meta

def _proc_exit_code(meta):
    proc = _proc_from_meta(meta)
    if not proc:
        return None
    return proc.poll()

def _remember_stream(canal, proc, player_type, calidad, audio_only, log):
    procesos[canal] = {
        "proc": proc,
        "player": player_type,
        "quality": calidad,
        "audioOnly": bool(audio_only),
        "startedAt": time.time(),
        "exitCode": None,
        "log": log,
        "lastError": ""
    }

def _quick_launch_error(proc, log, delay=0.8):
    time.sleep(delay)
    code = proc.poll()
    if code is None:
        return None
    tail = ""
    try:
        if os.path.exists(log):
            with open(log, "r", encoding="utf-8", errors="replace") as f:
                tail = f.read()[-900:].strip()
    except Exception:
        tail = ""
    return f"El reproductor se cerro al iniciar (exit {code}). {tail}".strip()

def lanzar_ventana(canal, calidad, token, mpv_path, sl_path, player_type="mpv", vlc_path="vlc", audio_only=False):
    vlc_path = vlc_gui_path(vlc_path)
    with lock:
        if canal in procesos:
            kill_proc(_proc_from_meta(procesos[canal]))
            del procesos[canal]

    _ensure_log_dirs()
    log    = _channel_log_path(canal)
    cflags = subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0

    if audio_only:
        # streamlink's "audio_only" quality is broken on Twitch (falls back to video).
        # Fix: get HLS URL directly (tries audio_only first, falls through to worst),
        # then launch player with --no-video as a real argument (not via --player-args string).
        print(f"  [{canal}] Solo-audio: obteniendo URL HLS...")
        hls_url, got_q = get_hls_url(canal, "audio_only", token, sl_path)
        if not hls_url:
            return False, "No se pudo obtener URL HLS"
        print(f"  [{canal}] Solo-audio con calidad={got_q}, lanzando player --no-video")
        if player_type == "vlc":
            cmd = [vlc_path, f"--meta-title={canal}", "--no-video", "--qt-minimal-view", "--no-video-title-show", "--no-one-instance",
                   "--no-qt-privacy-ask", "--no-qt-error-dialogs", "--no-media-library", hls_url]
        else:
            cmd = [mpv_path, "--no-video", f"--title={canal}",
                   "--force-window=yes", "--volume=100", "--audio-exclusive=no", hls_url]
        try:
            with open(log, "w", encoding="utf-8", errors="replace") as log_f:
                proc = popen_gui(cmd, stdout=log_f, stderr=subprocess.STDOUT)
            err = _quick_launch_error(proc, log)
            if err:
                return False, err
            with lock:
                _remember_stream(canal, proc, player_type, got_q or calidad, True, log)
            return True, "ok"
        except FileNotFoundError as e:
            return False, f"Ejecutable no encontrado: {e}"
        except Exception as e:
            return False, str(e)

    if player_type == "vlc":
        # Avoid Streamlink's --player-args quoting layer for VLC on Windows.
        # Bad quoting can make VLC open its console help ("vlc-help.txt") and wait for ENTER.
        print(f"  [{canal}] VLC directo: obteniendo URL HLS...")
        hls_url, got_q = get_hls_url(canal, calidad, token, sl_path)
        if not hls_url:
            return False, "No se pudo obtener URL HLS"
        cmd = [
            vlc_path,
            f"--meta-title={canal}",
            "--qt-minimal-view",
            "--no-video-title-show",
            "--no-qt-privacy-ask",
            "--no-one-instance",
            "--no-qt-error-dialogs",
            "--no-media-library",
            "--quiet",
            hls_url
        ]
        try:
            with open(log, "w", encoding="utf-8", errors="replace") as log_f:
                proc = popen_gui(cmd, stdout=log_f, stderr=subprocess.STDOUT)
            err = _quick_launch_error(proc, log)
            if err:
                return False, err
            with lock:
                _remember_stream(canal, proc, player_type, got_q or calidad, False, log)
            return True, "ok"
        except FileNotFoundError as e:
            return False, f"Ejecutable no encontrado: {e}"
        except Exception as e:
            return False, str(e)

    # Normal mode: streamlink pipes to player
    if player_type == "vlc":
        player_bin  = vlc_path
        player_args = "--qt-minimal-view --no-video-title-show --no-one-instance --no-qt-privacy-ask --no-qt-error-dialogs --no-media-library"
    else:
        player_bin  = mpv_path
        player_args = f"--title={canal} --force-window=yes --volume=70 --audio-exclusive=no"

    args = [sl_path]
    if token:
        args += [f"--twitch-api-header=Authorization={token}"]
    args += [
        "--twitch-low-latency",
        f"--player={player_bin}",
        f"--player-args={player_args}",
        f"twitch.tv/{canal}", calidad
    ]
    try:
        with open(log, "w", encoding="utf-8", errors="replace") as log_f:
            proc = subprocess.Popen(
                args,
                stdout=log_f, stderr=subprocess.STDOUT,
                creationflags=cflags
            )
        err = _quick_launch_error(proc, log)
        if err:
            return False, err
        with lock:
            _remember_stream(canal, proc, player_type, calidad, False, log)
        return True, "ok"
    except FileNotFoundError as e:
        return False, f"Ejecutable no encontrado: {e}"
    except Exception as e:
        return False, str(e)

def write_vlm_conf(video_pairs, audio_pairs, ancho, alto):
    """
    Genera mosaic-vlm.conf para VLC mosaic.
    video_pairs: [(canal, url), ...] — obtienen tile en el mosaico + audio
    audio_pairs: [(canal, url), ...] — solo audio, sin tile
    Retorna (cols, rows, tile_w, tile_h) basado solo en los tiles de video.
    """
    n_vid = len(video_pairs)
    cols  = 1 if n_vid<=1 else 2 if n_vid<=2 else 2 if n_vid<=4 else 3 if n_vid<=9 else 4
    rows  = -(-n_vid // cols) if n_vid else 1
    w     = ancho // cols if cols else ancho
    h     = alto  // rows if rows else alto

    lines = []
    for i, (canal, url) in enumerate(video_pairs):
        sid = f"sh_{canal}"
        # video → mosaic tile; audio → display local (novideo suprime ventana extra)
        # Sin select=: ambos destinos reciben todo; mosaic-bridge descarta audio,
        # display{novideo} descarta video → resultado: tile + audio.
        out = f'#duplicate{{dst=mosaic-bridge{{id={i+1},width={w},height={h}}},dst=display{{novideo}}}}'
        lines += [f'new {sid} broadcast enabled', f'setup {sid} input "{url}"',
                  f'setup {sid} output {out}', f'control {sid} play', '']

    # Canales solo-audio: audio local, sin tile de vídeo
    for canal, url in audio_pairs:
        sid = f"sh_{canal}"
        lines += [f'new {sid} broadcast enabled', f'setup {sid} input "{url}"',
                  f'setup {sid} output #display{{novideo}}', f'control {sid} play', '']

    _ensure_log_dirs()
    with open(VLM_CONF, 'w', encoding='utf-8') as f:
        f.write('\n'.join(lines))
    return cols, rows, w, h


def lanzar_grid(canales, calidad, token, mpv_path, sl_path, ffmpeg_path, ancho, alto,
                player_type="mpv", vlc_path="vlc", gp_path="gridplayer",
                hwaccel=False, hwgpu="nvidia", vlc_mosaic=True, canales_audio=[]):
    global grid_proc
    _ensure_log_dirs()
    vlc_path = vlc_gui_path(vlc_path)
    detener_grid()
    fallidos = []

    # GridPlayer resuelve streams internamente — sin ffmpeg
    if player_type == "gridplayer":
        all_ch = list(canales) + list(canales_audio)
        if not all_ch:
            return False, "Ningun canal disponible", [], []
        try:
            args = [gp_path] + [f"https://twitch.tv/{c}" for c in all_ch]
            print(f"  CMD: {' '.join(args)}")
            proc = subprocess.Popen(args)
            grid_proc = {
                "procs": (proc,),
                "player": "gridplayer",
                "startedAt": time.time(),
                "exitCode": None,
                "validos": all_ch,
                "audioOnly": list(canales_audio),
                "fallidos": fallidos,
                "log": _grid_log_path()
            }
            return True, f"GridPlayer con {len(all_ch)} streams", all_ch, fallidos
        except FileNotFoundError:
            return False, f"GridPlayer no encontrado en '{gp_path}'", [], []
        except Exception as e:
            return False, str(e), [], []

    # ── Resolver URLs en paralelo ────────────────────────────────────
    def _fetch_hls_task(canal, is_audio):
        q = "audio_only" if is_audio else calidad
        url, got_q = get_hls_url(canal, q, token, sl_path)
        return canal, url, got_q, is_audio

    urls, validos = [], []
    audio_pairs = []  # (canal, url) para canales solo-audio
    all_tasks = [(c, False) for c in canales] + [(c, True) for c in canales_audio]
    if all_tasks:
        print(f"  Resolviendo {len(all_tasks)} URLs en paralelo...")
        fetch_results = {}
        with ThreadPoolExecutor(max_workers=min(8, len(all_tasks))) as ex:
            fut_map = {ex.submit(_fetch_hls_task, c, ia): (c, ia) for c, ia in all_tasks}
            for fut in as_completed(fut_map):
                try:
                    canal, url, got_q, is_audio = fut.result()
                    fetch_results[(canal, is_audio)] = (url, got_q)
                except Exception as e:
                    c, ia = fut_map[fut]
                    fetch_results[(c, ia)] = (None, None)
                    print(f"  ERROR fetch {c}: {e}")
        for canal in canales:
            url, got_q = fetch_results.get((canal, False), (None, None))
            if url:
                urls.append(url); validos.append(canal)
                print(f"  OK {canal} ({got_q})")
            else:
                fallidos.append({"canal": canal, "reason": "No se pudo obtener URL HLS"})
                print(f"  FALLO {canal}")
        for canal in canales_audio:
            url, got_q = fetch_results.get((canal, True), (None, None))
            if url:
                audio_pairs.append((canal, url))
                print(f"  OK audio {canal} ({got_q})")
            else:
                fallidos.append({"canal": canal, "reason": "No se pudo obtener URL HLS audio"})
                print(f"  FALLO audio {canal}")

    n_video = len(urls)
    n_audio = len(audio_pairs)
    n_total = n_video + n_audio   # total de canales activos

    if n_total == 0:
        return False, "Ningun canal disponible", [], fallidos

    log  = _grid_log_path()
    cno  = subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0

    # ── VLC Mosaic via VLM ────────────────────────────────────────
    if player_type == "vlc" and vlc_mosaic:
        video_pairs = list(zip(validos, urls))
        cols_m, rows_m, _, _ = write_vlm_conf(video_pairs, audio_pairs, ancho, alto)

        # Canvas para el filtro mosaic: archivo de video negro en loop.
        # stdin pipe causa RAM leak (VLC bufferiza sin límite).
        # Usamos un archivo pequeño generado por ffmpeg + --input-repeat=-1.
        black_bg = _runtime_app_path("vlc_bg.ts")
        print("  Generando fondo negro para VLC mosaic...")
        gen = subprocess.run([
            ffmpeg_path, "-f", "lavfi", "-i",
            f"color=c=black:s={ancho}x{alto}:r=1",
            "-t", "3", "-vcodec", "libx264", "-preset", "ultrafast", "-crf", "51",
            "-an", "-y", black_bg
        ], capture_output=True, creationflags=cno)

        if not os.path.exists(black_bg) or os.path.getsize(black_bg) == 0:
            err = gen.stderr.decode("utf-8", errors="replace").strip()[:500]
            extra = f": {err}" if err else ""
            return False, f"No se pudo generar el fondo negro (ffmpeg fallo){extra}", [], fallidos

        vlc_args = [
            vlc_path,
            black_bg, "--input-repeat=-1",   # fondo negro en loop — sin RAM leak
            "--vlm-conf", VLM_CONF,
            "--video-filter=mosaic",
            f"--mosaic-width={ancho}",
            f"--mosaic-height={alto}",
            f"--mosaic-cols={cols_m}",
            f"--mosaic-rows={rows_m}",
            "--mosaic-keep-aspect-ratio",
            "--qt-minimal-view",
            "--no-video-title-show",
            "--no-qt-privacy-ask",
            "--no-qt-error-dialogs",
            "--no-media-library",
            "--meta-title=StreamHub",
        ]
        print(f"  VLC mosaic {cols_m}x{rows_m} ({n_video} vid + {n_audio} audio)")
        print(f"  VLM={VLM_CONF}")
        try:
            with open(log, "w", encoding="utf-8", errors="replace") as log_f:
                vlc = popen_gui(vlc_args, stdout=log_f, stderr=log_f)
            all_validos = validos + [c for c, _ in audio_pairs]
            grid_proc = {
                "procs": (vlc,),
                "player": "vlc",
                "startedAt": time.time(),
                "exitCode": None,
                "validos": all_validos,
                "audioOnly": [c for c, _ in audio_pairs],
                "fallidos": fallidos,
                "log": log
            }
            return True, f"VLC Mosaic {cols_m}x{rows_m} con {n_total} streams", all_validos, fallidos
        except FileNotFoundError as e:
            return False, f"Ejecutable no encontrado: {e}", [], fallidos
        except Exception as e:
            return False, str(e), [], fallidos

    # ── ffmpeg filter_complex (mpv / VLC fallback) ────────────────
    # Canales video: tiles reales
    # Canales audio-only: tile negro + audio en amix
    cols  = 1 if n_total<=1 else 2 if n_total<=2 else 2 if n_total<=4 else 3 if n_total<=9 else 4
    rows  = -(-n_total // cols)
    total = cols * rows
    w     = ancho // cols
    h     = alto  // rows

    parts = []
    for i in range(n_video):
        parts.append(f"[{i}:v]scale={w}:{h},setpts=PTS-STARTPTS[v{i}]")
    for i in range(n_audio):          # tile negro para cada canal solo-audio
        parts.append(f"color=black:size={w}x{h}:rate=30[v{n_video+i}]")
    for i in range(n_total, total):   # padding si la cuadrícula no es exacta
        parts.append(f"color=black:size={w}x{h}:rate=30[v{i}]")

    positions = [f"{(i%cols)*w}_{(i//cols)*h}" for i in range(total)]
    xin = "".join(f"[v{i}]" for i in range(total))
    fc  = ";".join(parts) + f";{xin}xstack=inputs={total}:layout={'|'.join(positions)}[vout]"

    # amix: canales video [0..n_video-1] + canales audio [n_video..n_total-1]
    n_amix = n_total
    filter_audio = "".join(f"[{i}:a]" for i in range(n_amix)) + f"amix=inputs={n_amix}:duration=longest[aout]"
    fc_completo  = fc + ";" + filter_audio

    _ff_threads = max(2, (os.cpu_count() or 4) // 2)
    ff_base = [ffmpeg_path, "-y", "-loglevel", "error", "-threads", str(_ff_threads)]
    for url in urls:                         # video
        ff_base += ["-i", url]
    for _, url in audio_pairs:               # solo-audio (ffmpeg lee video+audio; solo usamos audio)
        ff_base += ["-i", url]
    ff_base += ["-filter_complex", fc_completo, "-map", "[vout]", "-map", "[aout]"]

    HW_ENCODERS = {
        "nvidia": ["-c:v","h264_nvenc","-preset","p1","-tune","ll","-rc","vbr","-cq","23"],
        "amd":    ["-c:v","h264_amf","-quality","speed","-rc","vbr_peak","-qp_i","23","-qp_p","23"],
        "intel":  ["-c:v","h264_qsv","-preset","veryfast","-global_quality","23"],
    }
    if hwaccel and hwgpu in HW_ENCODERS:
        ff_base += HW_ENCODERS[hwgpu]
        print(f"  HW accel: {hwgpu} ({HW_ENCODERS[hwgpu][1]})")
    else:
        ff_base += ["-c:v","libx264","-preset","ultrafast","-tune","zerolatency","-crf","23"]

    ff_base += ["-c:a","aac","-ac","2"]

    try:
        log_f = open(log, "w", encoding="utf-8", errors="replace")

        if player_type == "vlc":
            # ffmpeg → VLC pipe (modo fallback)
            ff_args  = ff_base + ["-f", "mpegts", "pipe:1"]
            vlc_args = [vlc_path, "--meta-title=StreamHub", "--qt-minimal-view", "--no-video-title-show",
                        "--no-qt-privacy-ask", "--no-qt-error-dialogs", "--no-media-library",
                        f"--width={ancho}", f"--height={alto}", "-"]
            ff = subprocess.Popen(ff_args, stdout=subprocess.PIPE, stderr=log_f, creationflags=cno)
            pl = popen_gui(vlc_args, stdin=ff.stdout, stdout=log_f, stderr=log_f)
            ff.stdout.close()
            log_f.close()
        else:
            # mpv
            ff_args = ff_base + ["-f","matroska","pipe:1"]
            pl_args = [mpv_path, "--title=StreamHub", "--force-window=yes",
                       "--no-cache", "--volume=100", "-"]
            ff = subprocess.Popen(ff_args, stdout=subprocess.PIPE, stderr=log_f, creationflags=cno)
            pl = subprocess.Popen(pl_args, stdin=ff.stdout, stdout=log_f, stderr=log_f)
            ff.stdout.close()
            log_f.close()

        all_validos = validos + [c for c, _ in audio_pairs]
        grid_proc = {
            "procs": (ff, pl),
            "player": player_type,
            "startedAt": time.time(),
            "exitCode": None,
            "validos": all_validos,
            "audioOnly": [c for c, _ in audio_pairs],
            "fallidos": fallidos,
            "log": log
        }
        return True, f"Grid {cols}x{rows} con {n_total} streams ({n_audio} audio)", all_validos, fallidos
    except FileNotFoundError as e:
        return False, f"Ejecutable no encontrado: {e}", [], fallidos
    except Exception as e:
        return False, str(e), [], fallidos

def kill_proc(proc):
    if proc is None:
        return
    try:
        if sys.platform == "win32":
            subprocess.run(
                ["taskkill", "/F", "/T", "/PID", str(proc.pid)],
                capture_output=True,
                creationflags=subprocess.CREATE_NO_WINDOW
            )
        else:
            proc.terminate()
    except Exception:
        pass

def popen_gui(args, stdout=None, stderr=None, stdin=None, cwd=None):
    flags = 0
    if sys.platform == "win32":
        flags = subprocess.CREATE_NO_WINDOW
    popen = globals().get("_ORIG_SUBPROCESS_POPEN", subprocess.Popen)
    return popen(
        args,
        stdin=stdin if stdin is not None else subprocess.DEVNULL,
        stdout=stdout if stdout is not None else subprocess.DEVNULL,
        stderr=stderr if stderr is not None else subprocess.DEVNULL,
        cwd=cwd,
        creationflags=flags
    )

def vlc_gui_path(vlc_path):
    p = normalize_path_value(vlc_path, "vlc")
    if sys.platform != "win32":
        return p
    try:
        # Never call bare "vlc" on Windows. PATHEXT often resolves it to
        # vlc.com, the console wrapper that opens the black help window.
        # Search PATH manually for vlc.exe only, then use the configured path.
        for folder in os.environ.get("PATH", "").split(os.pathsep):
            folder = folder.strip('" ')
            if not folder:
                continue
            exe = os.path.join(folder, "vlc.exe")
            if os.path.isfile(exe):
                return exe
        if os.path.isdir(p):
            exe = os.path.join(p, "vlc.exe")
            if os.path.exists(exe):
                return exe
        base = os.path.basename(p).lower()
        if base == "vlc.com":
            exe = os.path.join(os.path.dirname(p), "vlc.exe")
            if os.path.exists(exe):
                return exe
        if base == "vlc" and os.path.exists(p + ".exe"):
            return p + ".exe"
    except Exception:
        pass
    return p

def is_vlc_console_wrapper(path_value):
    return sys.platform == "win32" and os.path.basename(str(path_value or "")).lower() in ("vlc", "vlc.com")

def _log_process_launch(source, args):
    try:
        _ensure_log_dirs()
        log = _app_log_path("process-launch.log")
        safe_args = sanitize_for_log(str(args))
        line = f"{time.strftime('%Y-%m-%d %H:%M:%S')} [{source}] {safe_args}\n"
        with open(log, "a", encoding="utf-8", errors="replace") as f:
            f.write(line)
    except Exception:
        pass

def _command_name(args):
    if isinstance(args, (list, tuple)) and args:
        return str(args[0])
    if isinstance(args, str):
        return args.split()[0] if args.split() else ""
    return ""

def _looks_like_vlc_command(args):
    cmd = _command_name(args)
    base = os.path.basename(cmd).lower().strip('"')
    return base in ("vlc", "vlc.exe", "vlc.com")

def _normalize_vlc_args(args):
    if not _looks_like_vlc_command(args):
        return args
    if isinstance(args, str):
        parts = args.split()
        if not parts:
            return args
        parts[0] = vlc_gui_path(parts[0])
        return parts
    out = list(args)
    out[0] = vlc_gui_path(out[0])
    return out

_ORIG_SUBPROCESS_POPEN = subprocess.Popen
_ORIG_SUBPROCESS_RUN = subprocess.run

def _safe_subprocess_run(args, *pargs, **kwargs):
    if _looks_like_vlc_command(args):
        norm = _normalize_vlc_args(args)
        _log_process_launch("subprocess.run:vlc", norm)
        flat = [str(x).lower() for x in (norm if isinstance(norm, (list, tuple)) else str(norm).split())]
        if any(x in ("--version", "-h", "--help", "-help") for x in flat):
            text_mode = kwargs.get("text") or kwargs.get("universal_newlines") or kwargs.get("encoding")
            out = "VLC executable found; version check skipped.\n" if text_mode else b"VLC executable found; version check skipped.\n"
            err = "" if text_mode else b""
            return subprocess.CompletedProcess(norm, 0, stdout=out, stderr=err)
        args = norm
    return _ORIG_SUBPROCESS_RUN(args, *pargs, **kwargs)

def _safe_subprocess_popen(args, *pargs, **kwargs):
    if _looks_like_vlc_command(args):
        args = _normalize_vlc_args(args)
        _log_process_launch("subprocess.Popen:vlc", args)
        if sys.platform == "win32":
            kwargs["creationflags"] = kwargs.get("creationflags", 0) | subprocess.CREATE_NO_WINDOW
    return _ORIG_SUBPROCESS_POPEN(args, *pargs, **kwargs)

subprocess.run = _safe_subprocess_run
subprocess.Popen = _safe_subprocess_popen

def _del_log(path):
    try:
        if os.path.exists(path):
            os.remove(path)
    except Exception:
        pass

def _cleanup_unused_logs(force=False):
    global _last_log_cleanup
    now = time.time()
    if not force and now - _last_log_cleanup < LOG_CLEANUP_INTERVAL:
        return
    _last_log_cleanup = now
    _ensure_log_dirs()

    with lock:
        active_logs = set()
        for c, meta in procesos.items():
            log = meta.get("log") if isinstance(meta, dict) else _channel_log_path(c)
            if log:
                active_logs.add(os.path.realpath(log))
        if grid_proc:
            if isinstance(grid_proc, dict):
                active_logs.add(os.path.realpath(grid_proc.get("log", _grid_log_path())))
            else:
                active_logs.add(os.path.realpath(_grid_log_path()))

    try:
        for folder in (LOG_STREAMS_DIR, BASE):
            for filename in os.listdir(folder):
                if not _is_channel_log_name(filename) and filename != "grid.log":
                    continue
                path = os.path.realpath(os.path.join(folder, filename))
                if os.path.commonpath([folder, path]) != folder or path in active_logs:
                    continue
                _del_log(path)
    except OSError:
        pass

def _grid_procs(meta=None):
    meta = grid_proc if meta is None else meta
    if not meta:
        return ()
    if isinstance(meta, dict):
        return tuple(meta.get("procs") or ())
    return tuple(meta)

def detener_grid():
    global grid_proc
    log = None
    if grid_proc:
        if isinstance(grid_proc, dict):
            log = grid_proc.get("log")
        else:
            log = _grid_log_path()
        for p in _grid_procs(grid_proc):
            kill_proc(p)
        grid_proc = None
    _del_log(log or _grid_log_path())

def cerrar_ventana(canal):
    stop_channel_watch(canal, forget=True)   # detener watch si estaba activo
    log = _channel_log_path(canal)
    with lock:
        if canal in procesos:
            meta = procesos[canal]
            if isinstance(meta, dict):
                log = meta.get("log") or log
            kill_proc(_proc_from_meta(procesos[canal]))
            del procesos[canal]
    if log:
        _del_log(log)

def cerrar_todos():
    _stop_all_watches()
    logs = []
    with lock:
        for meta in procesos.values():
            if isinstance(meta, dict) and meta.get("log"):
                logs.append(meta.get("log"))
            kill_proc(_proc_from_meta(meta))
        procesos.clear()
    detener_grid()
    for log in logs:
        _del_log(log)
    _cleanup_unused_logs(force=True)

def streams_activos():
    _cleanup_unused_logs()
    with lock:
        activos = []
        for c, meta in procesos.items():
            code = _proc_exit_code(meta)
            if code is None:
                activos.append(c)
            elif isinstance(meta, dict):
                meta["exitCode"] = code
        return activos

def streams_status():
    _cleanup_unused_logs()
    with lock:
        status = {}
        for c, meta in procesos.items():
            proc = _proc_from_meta(meta)
            code = proc.poll() if proc else None
            if isinstance(meta, dict):
                meta["exitCode"] = code
                status[c] = {
                    "running": code is None,
                    "exitCode": code,
                    "player": meta.get("player", ""),
                    "quality": meta.get("quality", ""),
                    "audioOnly": bool(meta.get("audioOnly")),
                    "startedAt": meta.get("startedAt"),
                    "log": os.path.basename(meta.get("log", f"{c}.log")),
                    "lastError": meta.get("lastError", "")
                }
            else:
                status[c] = {"running": code is None, "exitCode": code, "player": "", "startedAt": None}
        return status

def grid_activo():
    global grid_proc
    if not grid_proc: return False
    procs = _grid_procs(grid_proc)
    if any(p.poll() is not None for p in procs):
        if isinstance(grid_proc, dict):
            grid_proc["exitCode"] = next((p.poll() for p in procs if p.poll() is not None), None)
        else:
            grid_proc = None
        return False
    return True

def grid_status():
    global grid_proc
    if not grid_proc:
        return {"running": False, "exitCode": None, "startedAt": None, "player": "", "validos": [], "audioOnly": [], "fallidos": []}
    procs = _grid_procs(grid_proc)
    exit_code = next((p.poll() for p in procs if p.poll() is not None), None)
    running = exit_code is None
    if isinstance(grid_proc, dict):
        grid_proc["exitCode"] = exit_code
        return {
            "running": running,
            "exitCode": exit_code,
            "startedAt": grid_proc.get("startedAt"),
            "player": grid_proc.get("player", ""),
            "validos": grid_proc.get("validos", []),
            "audioOnly": grid_proc.get("audioOnly", []),
            "fallidos": grid_proc.get("fallidos", []),
            "log": os.path.basename(grid_proc.get("log", "grid.log"))
        }
    return {"running": running, "exitCode": exit_code, "startedAt": None, "player": "", "validos": [], "audioOnly": [], "fallidos": []}

# ── HTTP HANDLER ─────────────────────────────────────────────

class Handler(http.server.SimpleHTTPRequestHandler):

    def log_message(self, fmt, *args):
        if len(args) > 1 and args[1] not in ("200","304"):
            super().log_message(fmt, *args)

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

    def send_json(self, data, status=200):
        body = json.dumps(data, ensure_ascii=False).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.add_cors_headers()
        self.end_headers()
        try:
            self.wfile.write(body)
        except (ConnectionAbortedError, BrokenPipeError):
            pass

    def do_OPTIONS(self):
        if not is_allowed_origin(self.headers.get("Origin")):
            self.send_response(403)
            self.end_headers()
            return
        self.send_response(204)
        self.add_cors_headers()
        self.send_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        self.send_header("Access-Control-Allow-Headers", "Content-Type, X-StreamHub-CSRF, X-TwitchGrid-CSRF")
        self.end_headers()

    def read_body(self):
        try:
            length = int(self.headers.get("Content-Length", 0))
        except ValueError:
            return {}
        if length > 64 * 1024:
            return {}
        body   = self.rfile.read(length)
        try:    return json.loads(body)
        except: return {}

    def require_local_post(self):
        if not is_allowed_origin(self.headers.get("Origin")):
            self.send_json({"ok": False, "error": "Origen no permitido"}, 403)
            return False
        sent_csrf = self.headers.get("X-StreamHub-CSRF") or self.headers.get("X-TwitchGrid-CSRF")
        if sent_csrf != CSRF_TOKEN:
            self.send_json({"ok": False, "error": "CSRF invalido"}, 403)
            return False
        return True

    def serve_static_path(self, parsed_path):
        filename = STATIC_FILES.get(parsed_path)
        if not filename:
            self.send_error(404)
            return
        path = os.path.realpath(os.path.join(BASE, filename))
        if os.path.commonpath([BASE, path]) != BASE or not os.path.isfile(path):
            self.send_error(404)
            return
        try:
            with open(path, "rb") as f:
                body = f.read()
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.add_cors_headers()
            self.end_headers()
            self.wfile.write(body)
        except OSError:
            self.send_error(404)

    def do_GET(self):
        parsed = urlparse(self.path)
        qs     = parse_qs(parsed.query)

        if not is_allowed_origin(self.headers.get("Origin")):
            self.send_json({"ok": False, "error": "Origen no permitido"}, 403)
            return

        if parsed.path == "/api/status":
            stream_state = streams_status()
            gstate = grid_status()
            self.send_json({
                "activos": [c for c, s in stream_state.items() if s.get("running")],
                "streams": stream_state,
                "grid": gstate.get("running", False),
                "gridStatus": gstate,
                "csrf": CSRF_TOKEN
            })

        elif parsed.path == "/api/live":
            # Consulta estado en vivo de canales
            canales = normalize_channels(qs.get("canales", [""])[0].split(","))
            token, client_id = get_config_credentials()

            streams = get_streams_info(canales, token, client_id)
            users   = get_user_info(canales, token, client_id)

            result = {}
            for canal in canales:
                result[canal] = {
                    **users.get(canal, {"display_name": canal, "avatar": ""}),
                    **streams.get(canal, {"live": False, "title": "", "viewers": 0, "game": "", "thumbnail": "", "started_at": ""})
                }
            self.send_json(result)

        elif parsed.path == "/api/followed":
            token, client_id = get_config_credentials()
            self.send_json({"streams": get_followed_live(token, client_id)})

        elif parsed.path == "/api/categories":
            token, client_id = get_config_credentials()
            cursor    = qs.get("cursor",    [""])[0]
            cats, cur = get_top_categories(token, client_id, cursor)
            self.send_json({"categories": cats, "cursor": cur})

        elif parsed.path == "/api/recommended":
            token, client_id = get_config_credentials()
            self.send_json({"streams": get_recommended(token, client_id)})

        elif parsed.path == "/api/search_cats":
            token, client_id = get_config_credentials()
            query     = qs.get("q",         [""])[0]
            cats = search_categories(query, token, client_id)
            self.send_json({"categories": cats})

        elif parsed.path == "/api/cat_streams":
            token, client_id = get_config_credentials()
            game_id   = qs.get("game_id",   [""])[0]
            cursor    = qs.get("cursor",    [""])[0]
            streams, cur = get_category_streams(game_id, token, client_id, cursor)
            self.send_json({"streams": streams, "cursor": cur})

        elif parsed.path == "/api/token_status":
            expired = gql_is_token_expired()
            tok, _ = get_config_credentials()
            self.send_json({"expired": expired, "hasToken": bool(tok)})

        elif parsed.path == "/api/config":
            self.send_json(redacted_config())

        elif parsed.path == "/api/me":
            token, client_id = get_config_credentials()
            user = get_my_user(token, client_id)
            if not user:
                self.send_json({"ok": False, "error": "Usuario no disponible"}, 404)
            else:
                self.send_json({
                    "ok": True,
                    "login": str(user.get("login", "")).lower(),
                    "display_name": user.get("display_name", user.get("login", "")),
                })

        elif parsed.path == "/api/deps":
            self.send_json(dependency_status())

        elif parsed.path == "/api/logs":
            name = qs.get("name", ["grid"])[0]
            tail = read_log_tail(name)
            if tail is None:
                self.send_json({"ok": False, "error": "Log invalido"}, 400)
            else:
                self.send_json({"ok": True, "name": name, "log": tail})

        elif parsed.path in ("/api/mining/status", "/api/points", "/api/drops"):
            self.send_json(mining_status())

        elif parsed.path == "/api/mining/log":
            with mining_lock:
                log = list(mining_log[-50:])
            self.send_json({"events": log})

        else:
            self.serve_static_path(parsed.path)

    def do_POST(self):
        parsed = urlparse(self.path)
        if not self.require_local_post():
            return
        data   = self.read_body()

        if parsed.path == "/api/play":
            canal = normalize_channel(data.get("canal", ""))
            if not canal:
                self.send_json({"ok": False, "error": "Canal invalido"}, 400)
                return
            token, _client_id = get_config_credentials()
            ok, msg = lanzar_ventana(
                canal, normalize_quality(data.get("calidad", "best")), token,
                normalize_path_value(data.get("mpv"), "mpv"),
                normalize_path_value(data.get("streamlink"), "streamlink"),
                normalize_player(data.get("player", "mpv")),
                normalize_path_value(data.get("vlc"), "vlc"),
                audio_only=bool(data.get("audio_only", False))
            )
            self.send_json({"ok": ok, "canal": canal, "error": None if ok else msg, "state": streams_status().get(canal)})

        elif parsed.path == "/api/grid":
            print(f"  grid player={data.get('player')} gp={data.get('gp')} vlc_mosaic={data.get('vlc_mosaic',True)}")
            token, _client_id = get_config_credentials()
            canales = normalize_channels(data.get("canales", []))
            canales_audio = normalize_channels(data.get("canales_audio", []))
            if not canales and not canales_audio:
                self.send_json({"ok": False, "msg": "Ningun canal valido", "validos": []}, 400)
                return
            result = lanzar_grid(
                canales, normalize_quality(data.get("calidad", "best")),
                token,
                normalize_path_value(data.get("mpv"), "mpv"),
                normalize_path_value(data.get("streamlink"), "streamlink"),
                normalize_path_value(data.get("ffmpeg"), "ffmpeg"),
                clamp_int(data.get("ancho"), 1920, 320, 7680),
                clamp_int(data.get("alto"), 1080, 240, 4320),
                normalize_player(data.get("player", "mpv")),
                normalize_path_value(data.get("vlc"), "vlc"),
                normalize_path_value(data.get("gp"), "gridplayer"),
                hwaccel=bool(data.get("hwaccel",False)),
                hwgpu=data.get("hwgpu","nvidia") if data.get("hwgpu","nvidia") in HW_GPUS else "nvidia",
                vlc_mosaic=bool(data.get("vlc_mosaic", True)),
                canales_audio=canales_audio
            )
            if len(result) == 3:
                ok, msg, validos = result
                fallidos = []
            else:
                ok, msg, validos, fallidos = result
            self.send_json({"ok": ok, "msg": msg, "validos": validos, "fallidos": fallidos, "gridStatus": grid_status()})

        elif parsed.path == "/api/stop":
            canal = normalize_channel(data.get("canal", ""))
            if canal:
                cerrar_ventana(canal)
            else:
                cerrar_todos()
            self.send_json({"ok": True})

        elif parsed.path == "/api/stopgrid":
            detener_grid()
            self.send_json({"ok": True})

        elif parsed.path == "/api/stopall":
            cerrar_todos()
            self.send_json({"ok": True})

        elif parsed.path == "/api/config":
            ok = save_config(sanitize_config_update(data))
            self.send_json({"ok": ok})

        elif parsed.path == "/api/credentials/clear":
            ok = clear_credentials()
            self.send_json({"ok": ok})

        elif parsed.path == "/api/watch/start":
            canal = normalize_channel(data.get("canal", ""))
            if not canal:
                self.send_json({"ok": False, "error": "Canal invalido"}, 400)
                return
            ok, msg = start_channel_watch(canal)
            self.send_json({"ok": ok, "msg": msg, "canal": canal})

        elif parsed.path == "/api/watch/stop":
            canal = normalize_channel(data.get("canal", ""))
            if not canal:
                self.send_json({"ok": False, "error": "Canal invalido"}, 400)
                return
            ok, msg = stop_channel_watch(canal, forget=bool(data.get("forget", False)))
            self.send_json({"ok": ok, "msg": msg, "canal": canal})

        elif parsed.path == "/api/mining/start":
            ok, msg = start_mining()
            if ok:
                cfg = load_config()
                cfg["miningEnabled"] = True
                save_config(cfg)
            self.send_json({"ok": ok, "msg": msg, "status": mining_status()})

        elif parsed.path == "/api/mining/stop":
            ok, msg = stop_mining()
            cfg = load_config()
            cfg["miningEnabled"] = False
            save_config(cfg)
            self.send_json({"ok": ok, "msg": msg})

        elif parsed.path == "/api/shutdown":
            self.send_json({"ok": True, "msg": "Apagando servidor"})
            threading.Thread(target=lambda: (time.sleep(0.3), os._exit(0)), daemon=True).start()

        elif parsed.path == "/api/restart":
            self.send_json({"ok": True, "msg": "Reiniciando servidor"})
            cno = subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0
            def _do_restart():
                time.sleep(0.4)
                subprocess.Popen(
                    [sys.executable, os.path.abspath(__file__)],
                    cwd=BASE,
                    creationflags=cno
                )
                time.sleep(0.1)
                os._exit(0)
            threading.Thread(target=_do_restart, daemon=True).start()

        else:
            self.send_json({"ok": False, "error": "Ruta no encontrada"}, 404)


class ReuseServer(http.server.ThreadingHTTPServer):
    allow_reuse_address = True

def _ensure_default_config():
    """Crea config.json con valores por defecto si no existe."""
    if os.path.exists(CONFIG_FILE):
        return
    default = {
        "player": "mpv",
        "mpv": "mpv",
        "vlc": "vlc",
        "gp": "gridplayer",
        "ff": "ffmpeg",
        "sl": "streamlink",
        "quality": "480p",
        "w": 1920,
        "h": 1080,
        "hwaccel": False,
        "hwgpu": "nvidia",
        "vlcFfmpeg": False,
        "textOnly": False,
        "accentColor": "#e0553a",
        "clientId": "",
        "token": "",
    }
    save_config(default)
    print("  Config creada con valores por defecto.")

def _print_tool_status():
    cfg = load_config()
    tok, _ = get_config_credentials()
    tools = [
        ("streamlink", "sl",  None),
        ("ffmpeg",     "ff",  ["-version"]),
        ("mpv",        "mpv", ["--version"]),
        ("vlc",        "vlc", ["--version"]),
    ]
    print("  Herramientas:")
    for generic, cfg_key, vargs in tools:
        resolved, method = resolve_executable(generic, cfg.get(cfg_key, generic))
        r = check_command(generic, resolved, vargs)
        status = "OK" if r["ok"] else "NO ENCONTRADO"
        src    = f"[{method}]" if r["ok"] else ""
        print(f"    {generic:<12} {status} {src}")
    cid = cfg.get("clientId") or cfg.get("client_id", "")
    print(f"  Credenciales Twitch: clientId={'SI' if cid else 'NO'}  token={'SI' if tok else 'NO'}")

def _cleanup_all_procs():
    """atexit handler — terminate all spawned processes on exit."""
    try:
        cerrar_todos()
    except Exception:
        pass


def _setup_rotating_log():
    """Configure rotating file handler for app-level logging."""
    try:
        _ensure_log_dirs()
        handler = logging.handlers.RotatingFileHandler(
            _app_log_path("server.log"),
            maxBytes=10 * 1024 * 1024,
            backupCount=3,
            encoding="utf-8",
        )
        handler.setFormatter(logging.Formatter("%(asctime)s %(levelname)s %(message)s"))
        logging.getLogger().addHandler(handler)
        logging.getLogger().setLevel(logging.WARNING)
    except Exception:
        pass


def main():
    os.chdir(BASE)
    _ensure_default_config()
    _migrate_legacy_runtime_files()
    _cleanup_unused_logs(force=True)
    _setup_rotating_log()

    # Register cleanup on exit (zombie process prevention)
    atexit.register(_cleanup_all_procs)

    # Migrate plaintext token from config.json to secure storage
    cfg = load_config()
    if cfg.get("token") and SECURE_TOKEN_AVAILABLE:
        if migrate_from_plaintext(cfg["token"]):
            cfg.pop("token", None)
            save_config(cfg)
            print("  Token migrado a almacenamiento seguro.")

    # Configure GQL client ID from config (allows user override)
    gql_client_cfg = cfg.get("clientId") or cfg.get("client_id", "")
    if gql_client_cfg:
        gql_set_client(gql_client_cfg)

    server = ReuseServer((HOST, PORT), Handler)

    # Arrancar mining automáticamente si estaba habilitado
    cfg = load_config()
    if cfg.get("miningEnabled"):
        def _delayed_mining():
            time.sleep(1.5)   # esperar a que el servidor arranque
            ok, msg = start_mining()
            print(f"  [Mining] Auto-start: {msg}")
        threading.Thread(target=_delayed_mining, daemon=True).start()

    t = threading.Thread(target=server.serve_forever, daemon=True)
    t.start()

    print(f"""
  +==========================================+
  |      TWITCHGRID Server                   |
  +==========================================+
  http://{HOST}:{PORT}/twitch-multistream.html
  Ctrl+C para detener
""")
    _print_tool_status()
    print()

    try:
        while t.is_alive():
            t.join(timeout=0.5)
    except KeyboardInterrupt:
        print("\n  Cerrando...")
        cerrar_todos()
        os._exit(0)

if __name__ == "__main__":
    main()
