//! Eventos de 7TV en vivo (SEVENT).
//!
//! Protocolo idéntico al que usa Chatterino (`SevenTV/EventAPI`):
//! - WebSocket `wss://7tv.io/ws?platform=twitch&channel_id={id}`.
//! - Opcodes: `0` Dispatch, `1` Hello, `2` Heartbeat, `35` Subscribe.
//! - Suscripción: `{"op":35,"d":{"type":"UpdateEmoteSet","condition":{"ctx":"channel","platform":"TWITCH","id":"<id>"}}}`.
//!
//! Sirve para ver en directo los emotes que se añaden a un canal, los raids y
//! los CAST (recompensas) de 7TV.

use std::io::ErrorKind;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tungstenite::Message;

use crate::seven_tv;
use crate::ws;

const HOST: &str = "7tv.io";

/// Evento recibido del WebSocket de 7TV.
#[derive(Clone, Debug)]
pub enum Event {
    /// Estado de la conexión.
    Connected(bool),
    /// El conjunto de emotes de un canal ha cambiado.
    EmotesUpdated {
        channel: String,
        emotes: Vec<seven_tv::SevenEmote>,
    },
    /// Recompensa (CAST) de 7TV en el canal.
    Cosmetic {
        channel: String,
        name: String,
    },
}

/// Arranca el hilo de eventos de 7TV para un canal.
pub fn spawn(
    channel_id: String,
    events: Sender<Event>,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("7tv-events".into())
        .spawn(move || {
            let path = format!("/ws?platform=twitch&channel_id={channel_id}");
            let mut attempts: u32 = 0;
            while !stop.load(Ordering::Relaxed) {
                match ws::connect(HOST, &path, Duration::from_secs(10)) {
                    Ok(mut socket) => {
                        attempts = 0;
                        let _ = events.send(Event::Connected(true));
                        // Suscripción a los cambios del conjunto de emotes.
                        let subscribe = serde_json::json!({
                            "op": 35,
                            "d": {
                                "type": "UpdateEmoteSet",
                                "condition": {
                                    "ctx": "channel",
                                    "platform": "TWITCH",
                                    "id": channel_id
                                }
                            }
                        });
                        let _ = socket.send(Message::Text(subscribe.to_string().into()));

                        let mut heartbeat = Duration::from_secs(30);
                        let mut last_heartbeat = Instant::now();
                        while !stop.load(Ordering::Relaxed) {
                            // 7TV exige enviar un latido con la periodicidad
                            // que indica el saludo (op 1).
                            if last_heartbeat.elapsed() >= heartbeat {
                                let _ = socket.send(Message::Text(
                                    "{\"op\":2,\"d\":null}".into(),
                                ));
                                last_heartbeat = Instant::now();
                            }
                            match socket.read() {
                                Ok(Message::Text(text)) => {
                                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                                        let op = value.get("op").and_then(|v| v.as_u64()).unwrap_or(0);
                                        match op {
                                            // Hello: indica cada cuánto latir.
                                            1 => {
                                                if let Some(interval) = value
                                                    .pointer("/d/heartbeat_interval")
                                                    .and_then(|v| v.as_u64())
                                                {
                                                    heartbeat =
                                                        Duration::from_millis(interval.max(5000));
                                                }
                                            }
                                            // Dispatch: evento real.
                                            0 => dispatch(&value, &channel_id, &events),
                                            _ => {}
                                        }
                                    }
                                }
                                Ok(Message::Ping(payload)) => {
                                    let _ = socket.send(Message::Pong(payload));
                                }
                                Ok(Message::Close(_)) => break,
                                Ok(_) => {}
                                Err(tungstenite::Error::Io(err))
                                    if err.kind() == ErrorKind::WouldBlock
                                        || err.kind() == ErrorKind::TimedOut => {}
                                Err(_) => break,
                            }
                        }
                        let _ = socket.close(None);
                        let _ = events.send(Event::Connected(false));
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                    }
                    Err(_) => {
                        let _ = events.send(Event::Connected(false));
                    }
                }
                attempts = attempts.saturating_add(1);
                let wait = Duration::from_secs((5 * attempts).min(60) as u64);
                let deadline = Instant::now() + wait;
                while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        })
        .expect("no se pudo crear el hilo de eventos de 7TV")
}

/// Traduce un dispatch de 7TV.
fn dispatch(
    value: &serde_json::Value,
    channel_id: &str,
    events: &Sender<Event>,
) {
    let Some(kind) = value.pointer("/d/type").and_then(|v| v.as_str()) else {
        return;
    };
    let body = value.pointer("/d/body");
    match kind {
        "UpdateEmoteSet" | "CreateEmoteSet" | "AnyEmoteSet" => {
            let Some(body) = body else { return };
            let emotes = seven_tv::parse_emotes(body);
            if !emotes.is_empty() {
                let _ = events.send(Event::EmotesUpdated {
                    channel: channel_id.to_string(),
                    emotes,
                });
            }
        }
        "CreateCosmetic" | "UpdateCosmetic" | "AnyCosmetic" => {
            let Some(body) = body else { return };
            let name = body
                .pointer("/cosmetic/name")
                .and_then(|v| v.as_str())
                .or_else(|| body.get("name").and_then(|v| v.as_str()))
                .unwrap_or("recompensa")
                .to_string();
            let _ = events.send(Event::Cosmetic {
                channel: channel_id.to_string(),
                name,
            });
        }
        _ => {}
    }
}
