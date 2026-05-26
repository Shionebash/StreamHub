#!/usr/bin/env python3
"""
StreamHub - Twitch PubSub WebSocket Manager
Suscribe a: community-points-user-v1 y user-drop-events
Usa threading (no asyncio) para ser compatible con server.py.

Requiere: install.bat o bash install.sh
"""

import json
import secrets
import threading
import time

try:
    import websocket
    WEBSOCKET_AVAILABLE = True
except ImportError:
    WEBSOCKET_AVAILABLE = False

PUBSUB_URL     = "wss://pubsub-edge.twitch.tv/v1"
PING_INTERVAL  = 240         # segundos entre PINGs (Twitch requiere <= 5 min)
PONG_TIMEOUT   = 15          # segundos máx para recibir PONG
RECONNECT_BASE = 2           # segundos base para backoff
RECONNECT_MAX  = 60          # máx espera entre reconexiones


class PubSubManager:
    """Gestiona la conexión WebSocket a Twitch PubSub.

    Suscribe a los topics de puntos de canal y drops del usuario.
    Llama a on_event_cb(event_type, data) en cada evento relevante.

    event_type valores:
      'claim-available'  → data: {channel_id, channel_login, claim_id, points_earned}
      'points-earned'    → data: {channel_login, balance, points_earned, reason_code}
      'drop-progress'    → data: {drop_id, current_minutes, required_minutes, channel_login}
      'drop-claim'       → data: {drop_instance_id, channel_login}
      'status'           → data: {connected: bool, reason: str}
    """

    def __init__(self, token, user_id, on_event_cb):
        if not WEBSOCKET_AVAILABLE:
            raise RuntimeError(
                "websocket-client no instalado. Ejecuta install.bat o bash install.sh"
            )
        self._token       = token
        self._user_id     = str(user_id)
        self._on_event    = on_event_cb
        self._ws          = None
        self._running     = False
        self._connected   = False
        self._pong_recv   = threading.Event()
        self._stop_event  = threading.Event()
        self._thread      = None
        self._ping_thread = None
        self._reconnect_n = 0

    # ── PUBLIC ────────────────────────────────────────────────────

    @property
    def connected(self):
        return self._connected

    @property
    def running(self):
        return self._running

    def start(self):
        if self._running:
            return
        self._running = True
        self._stop_event.clear()
        self._thread = threading.Thread(target=self._run_loop, daemon=True, name="PubSubMain")
        self._thread.start()

    def stop(self):
        self._running    = False
        self._connected  = False
        self._stop_event.set()
        if self._ws:
            try:
                self._ws.close()
            except Exception:
                pass
        self._on_event("status", {"connected": False, "reason": "stopped"})

    # ── INTERNALS ─────────────────────────────────────────────────

    def _topics(self):
        uid = self._user_id
        return [
            f"community-points-user-v1.{uid}",
            f"user-drop-events.{uid}",
        ]

    def _run_loop(self):
        while self._running and not self._stop_event.is_set():
            try:
                self._connect_and_listen()
            except Exception as e:
                print(f"  [PubSub] Error en run_loop: {e}")
            if not self._running:
                break
            delay = min(RECONNECT_BASE * (2 ** self._reconnect_n), RECONNECT_MAX)
            self._reconnect_n += 1
            print(f"  [PubSub] Reconectando en {delay}s...")
            self._stop_event.wait(delay)

    def _connect_and_listen(self):
        self._connected = False
        self._pong_recv.clear()

        ws = websocket.WebSocketApp(
            PUBSUB_URL,
            on_open    = self._on_open,
            on_message = self._on_message,
            on_error   = self._on_error,
            on_close   = self._on_close,
        )
        self._ws = ws
        ws.run_forever(ping_interval=0)   # ping manual via hilo propio

    def _on_open(self, ws):
        self._connected  = True
        self._reconnect_n = 0
        print(f"  [PubSub] Conectado a {PUBSUB_URL}")
        self._subscribe(self._topics())
        self._on_event("status", {"connected": True, "reason": "connected"})
        # Iniciar hilo de ping
        self._ping_thread = threading.Thread(
            target=self._ping_loop, daemon=True, name="PubSubPing"
        )
        self._ping_thread.start()

    def _on_close(self, ws, code, msg):
        self._connected = False
        print(f"  [PubSub] Conexión cerrada (code={code})")
        self._on_event("status", {"connected": False, "reason": f"closed:{code}"})

    def _on_error(self, ws, error):
        print(f"  [PubSub] WebSocket error: {error}")

    def _subscribe(self, topics):
        msg = {
            "type": "LISTEN",
            "nonce": secrets.token_hex(8),
            "data": {
                "topics":    topics,
                "auth_token": self._token,
            },
        }
        try:
            self._ws.send(json.dumps(msg))
            print(f"  [PubSub] Suscrito a {len(topics)} topics: {topics}")
        except Exception as e:
            print(f"  [PubSub] Error al suscribir: {e}")

    def _ping_loop(self):
        while self._connected and not self._stop_event.is_set():
            self._stop_event.wait(PING_INTERVAL)
            if not self._connected:
                break
            try:
                self._pong_recv.clear()
                self._ws.send(json.dumps({"type": "PING"}))
                if not self._pong_recv.wait(PONG_TIMEOUT):
                    print("  [PubSub] PONG timeout — cerrando para reconectar")
                    self._ws.close()
                    break
            except Exception as e:
                print(f"  [PubSub] Error en ping_loop: {e}")
                break

    def _on_message(self, ws, raw):
        try:
            msg = json.loads(raw)
        except Exception:
            return

        msg_type = msg.get("type", "")

        if msg_type == "PONG":
            self._pong_recv.set()
            return

        if msg_type == "RECONNECT":
            print("  [PubSub] Servidor pidió reconexión")
            ws.close()
            return

        if msg_type == "RESPONSE":
            err = msg.get("error", "")
            nonce = msg.get("nonce", "")
            if err:
                print(f"  [PubSub] Error de suscripción [{nonce}]: {err}")
            else:
                print(f"  [PubSub] Suscripción OK [{nonce}]")
            return

        if msg_type != "MESSAGE":
            return

        topic   = msg.get("data", {}).get("topic", "")
        payload_raw = msg.get("data", {}).get("message", "{}")
        try:
            payload = json.loads(payload_raw)
        except Exception:
            return

        if topic.startswith("community-points-user-v1"):
            print(f"  [PubSub] pts event: {payload.get('type','?')} | {str(payload)[:200]}")
            self._handle_points(payload)
        elif topic.startswith("user-drop-events"):
            print(f"  [PubSub] drop event: {payload.get('type','?')} | {str(payload)[:200]}")
            self._handle_drops(payload)

    # ── EVENT HANDLERS ────────────────────────────────────────────

    def _handle_points(self, payload):
        msg_type = payload.get("type", "")

        if msg_type == "claim-available":
            try:
                claim         = payload["data"]["claim"]
                channel_id    = str(claim.get("channel_id", ""))
                # channel_login no siempre está en el claim; usar channel_id como fallback
                channel_login = claim.get("channel_login", "") or f"__id_{channel_id}"
                claim_id      = claim.get("id", "")
                points_earned = claim.get("point_gain", {}).get("total_points", 0)
                self._on_event("claim-available", {
                    "channel_id":    channel_id,
                    "channel_login": channel_login,
                    "claim_id":      claim_id,
                    "points_earned": points_earned,
                })
            except (KeyError, TypeError) as e:
                print(f"  [PubSub] Error parseando claim-available: {e} | {payload}")

        elif msg_type == "points-earned":
            try:
                data          = payload["data"]
                channel_login = data.get("channel_login", "") or f"__id_{data.get('channel_id','')}"
                # balance está en data["balance"]["balance"], NO en data["point_gain"]["balance"]
                balance       = data.get("balance", {}).get("balance", 0)
                earned        = data.get("point_gain", {}).get("total_points", 0)
                reason        = data.get("point_gain", {}).get("reason_code", "")
                self._on_event("points-earned", {
                    "channel_login": channel_login,
                    "channel_id":    str(data.get("channel_id", "")),
                    "balance":       balance,
                    "points_earned": earned,
                    "reason_code":   reason,
                })
            except (KeyError, TypeError) as e:
                print(f"  [PubSub] Error parseando points-earned: {e}")

        elif msg_type == "community-moment-start":
            # Momento de clip para reclamar
            try:
                data        = payload["data"]
                moment_id   = data.get("moment_id", "")
                channel_id  = data.get("channel_id", "")
                self._on_event("moment-available", {
                    "moment_id":  moment_id,
                    "channel_id": channel_id,
                })
            except (KeyError, TypeError):
                pass

    def _handle_drops(self, payload):
        msg_type = payload.get("type", "")

        if msg_type == "drop-progress":
            try:
                data  = payload["data"]
                drop_id  = data.get("drop_id", "")
                current  = data.get("current_progress_min", 0)
                required = data.get("required_progress_min", 0)
                channel  = data.get("channel_login", "")
                self._on_event("drop-progress", {
                    "drop_id":          drop_id,
                    "current_minutes":  current,
                    "required_minutes": required,
                    "channel_login":    channel,
                    "percent":          round(current / required * 100) if required else 0,
                })
            except (KeyError, TypeError) as e:
                print(f"  [PubSub] Error parseando drop-progress: {e}")

        elif msg_type == "drop-claim":
            try:
                data             = payload["data"]
                drop_instance_id = data.get("drop_instance_id", "")
                channel          = data.get("channel_login", "")
                self._on_event("drop-claim", {
                    "drop_instance_id": drop_instance_id,
                    "channel_login":    channel,
                })
            except (KeyError, TypeError) as e:
                print(f"  [PubSub] Error parseando drop-claim: {e}")
