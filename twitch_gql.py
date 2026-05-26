#!/usr/bin/env python3
"""
StreamHub - GraphQL Client
Queries y mutations para Channel Points y Drops.
Usa el Client-ID web de Twitch (mismo que usan TwitchDropsMiner / ChannelPointsMiner).
"""

import base64
import json
import urllib.parse
import urllib.request
import urllib.error

SPADE_URL = "https://spade.twitch.tv/track"

GQL_URL         = "https://gql.twitch.tv/gql"
_GQL_CLIENT_DEFAULT = "kimne78kx3ncx6brgo4mv6wki5h1ko"   # Twitch web client ID (public, used by all Twitch web tools)
_gql_client     = _GQL_CLIENT_DEFAULT
USER_AGENT      = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36"
LAST_ERROR      = ""
TOKEN_EXPIRED   = False


def set_gql_client(client_id):
    """Override the GQL client ID (called by server.py on startup with config value)."""
    global _gql_client
    cid = str(client_id or "").strip()
    if cid:
        _gql_client = cid


def get_gql_client():
    return _gql_client


# ── CORE REQUEST ─────────────────────────────────────────────────

def _set_last_error(message):
    global LAST_ERROR
    LAST_ERROR = str(message or "")


def get_last_error():
    return LAST_ERROR


def is_token_expired():
    return TOKEN_EXPIRED


def reset_token_expired():
    global TOKEN_EXPIRED
    TOKEN_EXPIRED = False


def gql_request(payload, token, timeout=10):
    """POST al endpoint GQL. payload puede ser dict o list (batch).
    Devuelve la respuesta parseada o None en error."""
    global TOKEN_EXPIRED
    _set_last_error("")
    body = json.dumps(payload).encode("utf-8")
    req  = urllib.request.Request(
        GQL_URL,
        data=body,
        headers={
            "Client-ID":     _gql_client,
            "Authorization": f"Bearer {token}",
            "Content-Type":  "application/json",
            "User-Agent":    USER_AGENT,
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return json.loads(r.read())
    except urllib.error.HTTPError as e:
        body_bytes = e.read()[:500]
        _set_last_error(f"HTTP {e.code}: {body_bytes!r}")
        if e.code == 401:
            TOKEN_EXPIRED = True
            print(f"  [GQL] 401 Unauthorized — token may be expired")
        else:
            print(f"  [GQL] HTTP {e.code}: {body_bytes[:200]}")
        return None
    except Exception as e:
        _set_last_error(str(e))
        print(f"  [GQL] Error: {e}")
        return None


# ── CHANNEL POINTS ────────────────────────────────────────────────

def claim_channel_points(channel_id, claim_id, token):
    """Reclama un bonus chest de puntos de canal.
    channel_id: ID numérico del canal (string).
    claim_id:   claim ID recibido via PubSub 'claim-available'.
    Devuelve el nuevo balance (int) o None si falla.
    Nota: si el claim ya fue reclamado (por ej. por el navegador),
    Twitch puede retornar null en currentPoints — se considera OK."""
    payload = {
        "query": """mutation ClaimCommunityPoints($input: ClaimCommunityPointsInput!) {
            claimCommunityPoints(input: $input) {
                currentPoints
                claim { id }
            }
        }""",
        "variables": {
            "input": {
                "channelID": str(channel_id),
                "claimID":   str(claim_id),
            }
        },
    }
    resp = gql_request(payload, token)
    if not resp:
        return None
    # Si hay errores GQL pero no hay data, reportar fallo
    if resp.get("errors") and not resp.get("data"):
        errs = [e.get("message","") for e in resp["errors"]]
        _set_last_error("; ".join(errs))
        print(f"  [GQL] ClaimPoints error: {errs}")
        return None
    try:
        result = resp["data"]["claimCommunityPoints"]
        if result is None:
            # null = ya fue reclamado; tratar como éxito (puntos llegaron via PubSub)
            return -1
        return result.get("currentPoints") or -1
    except (KeyError, TypeError):
        return None


def get_points_balance(channel_login, token):
    """Obtiene el balance actual de puntos de canal via GQL inline.
    Devuelve int o None (None si el token no tiene contexto de usuario en GQL)."""
    # Nota: solo funciona si el token fue emitido con el client-id web de Twitch.
    # Tokens de apps terceras retornan self=null. En ese caso los puntos solo
    # se actualizan via PubSub points-earned events.
    payload = {
        "query": """query ChannelPoints($login: String!) {
            user(login: $login) {
                channel {
                    self {
                        communityPoints { balance }
                    }
                }
            }
        }""",
        "variables": {"login": channel_login},
    }
    resp = gql_request(payload, token)
    if not resp:
        return None
    try:
        pts = resp["data"]["user"]["channel"]["self"]["communityPoints"]
        return pts["balance"] if pts else None
    except (KeyError, TypeError):
        return None


def get_channel_id(channel_login, token):
    """Obtiene el ID numérico de un canal a partir del login."""
    payload = {
        "operationName": "ReportMenuItem",
        "variables": {"channelLogin": channel_login},
        "extensions": {
            "persistedQuery": {
                "version": 1,
                "sha256Hash": "8f3628981255345ca5e5453dfd84edb3a7b814f76d0cfd5e5f2b8fb55a04c7f9",
            }
        },
    }
    resp = gql_request(payload, token)
    if not resp:
        return None
    try:
        return resp["data"]["user"]["id"]
    except (KeyError, TypeError):
        return None


# ── DROPS ─────────────────────────────────────────────────────────

def claim_drop(drop_instance_id, token):
    """Reclama un drop. drop_instance_id viene del evento PubSub 'drop-claim'.
    Devuelve True si ok, False si error."""
    payload = {
        "operationName": "DropsPage_ClaimDropRewards",
        "variables": {"input": {"dropInstanceID": str(drop_instance_id)}},
        "extensions": {
            "persistedQuery": {
                "version": 1,
                "sha256Hash": "2f884fa187b8fadb2a49db0adc033e636f7b6aaee6e76de1e2bba9a7baf0daf6",
            }
        },
    }
    resp = gql_request(payload, token)
    if not resp:
        return False
    try:
        status = resp["data"]["claimDropRewards"]
        return status is not None
    except (KeyError, TypeError):
        return False


def get_drop_progress(channel_login, token):
    """Obtiene el progreso del drop activo en un canal.
    Devuelve dict {name, current_minutes, required_minutes, campaign, drop_id} o None."""
    payload = {
        "operationName": "VideoPlayerStreamInfoOverlayChannel",
        "variables": {"channel": channel_login},
        "extensions": {
            "persistedQuery": {
                "version": 1,
                "sha256Hash": "a5f2e34d626a9f4f5c0204f910bab2194948a9502089be558bb6e779a9e1b3d2",
            }
        },
    }
    resp = gql_request(payload, token)
    if not resp:
        return None
    try:
        drop_info = resp["data"]["user"]["dropCampaign"]
        if not drop_info:
            return None
        return {
            "name":             drop_info.get("name", ""),
            "campaign":         drop_info.get("name", ""),
            "current_minutes":  0,
            "required_minutes": 0,
            "drop_id":          drop_info.get("id", ""),
        }
    except (KeyError, TypeError):
        return None


def get_inventory_drops(token):
    """Obtiene todos los drops en progreso desde el inventario del usuario.
    Devuelve lista de dicts o []."""
    payload = {
        "operationName": "Inventory",
        "variables": {"fetchRewardCampaigns": True},
        "extensions": {
            "persistedQuery": {
                "version": 1,
                "sha256Hash": "37fea486d6179047c41d0f549088a4c3a7dd60c05c70956a1490262f532d0836",
            }
        },
    }
    resp = gql_request(payload, token)
    if not resp:
        return []
    drops = []
    try:
        campaigns = resp["data"]["currentUser"]["inventory"]["dropCampaignsInProgress"] or []
        for campaign in campaigns:
            for timed_drop in (campaign.get("timeBasedDrops") or []):
                self_drop = timed_drop.get("self", {})
                required  = timed_drop.get("requiredMinutesWatched", 0)
                current   = (self_drop.get("currentMinutesWatched") or 0)
                claim_id  = self_drop.get("dropInstanceID")
                drops.append({
                    "drop_id":          timed_drop.get("id", ""),
                    "name":             timed_drop.get("name", ""),
                    "campaign":         campaign.get("name", ""),
                    "game":             (campaign.get("game") or {}).get("name", ""),
                    "current_minutes":  current,
                    "required_minutes": required,
                    "percent":          round(current / required * 100) if required else 0,
                    "claimable":        bool(claim_id and current >= required),
                    "claim_id":         claim_id,
                    "claimed":          bool(self_drop.get("isClaimed")),
                    "ends_at":          timed_drop.get("endAt", ""),
                })
    except (KeyError, TypeError):
        pass
    return drops


# ── WATCH HEARTBEAT ───────────────────────────────────────────────

def get_stream_watch_info(channel_login, token):
    """Obtiene channel_id, broadcast_id y game_id para el heartbeat minute-watched.
    Devuelve dict {channel_id, broadcast_id, game_id, game_name} o None si no está en vivo."""
    payload = {
        "operationName": "VideoPlayerStreamInfoOverlayChannel",
        "variables": {"channel": channel_login},
        "extensions": {
            "persistedQuery": {
                "version": 1,
                "sha256Hash": "a5f2e34d626a9f4f5c0204f910bab2194948a9502089be558bb6e779a9e1b3d2",
            }
        },
    }
    resp = gql_request(payload, token)
    if not resp:
        return None
    try:
        user   = resp["data"]["user"]
        stream = user.get("stream")
        if not stream:
            return None          # canal offline
        game   = stream.get("game") or {}
        return {
            "channel_id":   str(user["id"]),
            "broadcast_id": str(stream["id"]),
            "game_id":      str(game.get("id", "")),
            "game_name":    game.get("name", ""),
        }
    except (KeyError, TypeError):
        return None


def send_minute_watched(channel_id, broadcast_id, user_id, game_id, token):
    """Envía el heartbeat minute-watched al endpoint Spade de Twitch.
    Twitch lo cuenta como 1 minuto de visualización → acumula puntos y drops.
    Devuelve True si HTTP 204, False si error."""
    payload = [{
        "event": "minute-watched",
        "properties": {
            "broadcast_id": broadcast_id,
            "channel_id":   channel_id,
            "player":       "site",
            "user_id":      user_id,
            "game":         game_id,
            "live":         True,
        },
    }]
    encoded = base64.b64encode(json.dumps(payload).encode()).decode()
    body    = f"data={urllib.parse.quote(encoded)}".encode()
    req = urllib.request.Request(
        SPADE_URL,
        data=body,
        headers={
            "Content-Type":  "application/x-www-form-urlencoded",
            "Authorization": f"Bearer {token}",
            "User-Agent":    USER_AGENT,
            "Referer":       "https://www.twitch.tv/",
            "Origin":        "https://www.twitch.tv",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=8) as r:
            return r.status in (200, 204)
    except urllib.error.HTTPError as e:
        if e.code in (200, 204):
            return True
        print(f"  [Watch] Spade HTTP {e.code} para channel_id={channel_id}")
        return False
    except Exception as e:
        print(f"  [Watch] Error Spade: {e}")
        return False
