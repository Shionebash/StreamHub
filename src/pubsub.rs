//! Cliente PubSub de Twitch: eventos de puntos de canal y de drops.
//!
//! Una sola conexión WebSocket con dos topics, como en la versión Python.

use std::io::ErrorKind;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tungstenite::Message;

use crate::ws;

pub const PUBSUB_HOST: &str = "pubsub-edge.twitch.tv";
pub const PUBSUB_PATH: &str = "/v1";

/// Mensaje de PubSub ya desatado.
#[derive(Clone, Debug)]
pub struct PubSubMessage {
    pub topic: String,
    pub payload: Value,
}

/// Estado de la conexión PubSub.
#[derive(Clone, Debug)]
pub enum Status {
    Connected,
    Disconnected,
    AuthRejected(String),
}

/// Arranca el hilo de PubSub para el usuario indicado.
pub fn spawn(
    token: String,
    user_id: String,
    events: Sender<PubSubMessage>,
    status: Sender<Status>,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("pubsub".into())
        .spawn(move || {
            let clean_token = token
                .trim()
                .strip_prefix("oauth:")
                .unwrap_or(token.trim())
                .to_string();
            let topics = vec![
                format!("community-points-user-v1.{user_id}"),
                format!("user-drop-events.{user_id}"),
            ];
            let mut attempts: u32 = 0;
            let mut was_connected = false;
            while !stop.load(Ordering::Relaxed) {
                match ws::connect(PUBSUB_HOST, PUBSUB_PATH, Duration::from_secs(10)) {
                    Ok(mut socket) => {
                        attempts = 0;
                        let listen_nonce = nonce();
                        let listen = serde_json::json!({
                            "type": "LISTEN",
                            "nonce": listen_nonce,
                            "data": {"topics": topics, "auth_token": clean_token}
                        });
                        if socket.send(Message::Text(listen.to_string().into())).is_err() {
                            let _ = socket.close(None);
                            if was_connected {
                                was_connected = false;
                                let _ = status.send(Status::Disconnected);
                            }
                            continue;
                        }
                        let mut last_ping = Instant::now();
                        let mut waiting_pong: Option<Instant> = None;
                        let mut reconnect = false;
                        let mut auth_rejected = false;
                        while !stop.load(Ordering::Relaxed) {
                            if let Some(sent_at) = waiting_pong {
                                if sent_at.elapsed() > Duration::from_secs(15) {
                                    reconnect = true;
                                    break;
                                }
                            } else if last_ping.elapsed() > Duration::from_secs(120) {
                                if socket
                                    .send(Message::Text("{\"type\":\"PING\"}".into()))
                                    .is_err()
                                {
                                    break;
                                }
                                last_ping = Instant::now();
                                waiting_pong = Some(last_ping);
                            }
                            match socket.read() {
                                Ok(Message::Text(text)) => {
                                    if let Ok(value) = serde_json::from_str::<Value>(&text) {
                                        match value.get("type").and_then(|t| t.as_str()) {
                                            Some("RECONNECT") => {
                                                reconnect = true;
                                                break;
                                            }
                                            Some("PONG") => {
                                                waiting_pong = None;
                                                continue;
                                            }
                                            Some("RESPONSE") => {
                                                let err = value
                                                    .get("error")
                                                    .and_then(|e| e.as_str())
                                                    .unwrap_or("");
                                                if err.is_empty() {
                                                    if !was_connected {
                                                        was_connected = true;
                                                        let _ = status.send(Status::Connected);
                                                    }
                                                } else if err == "ERR_BADAUTH" {
                                                    auth_rejected = true;
                                                    let _ = status.send(Status::AuthRejected(
                                                        err.to_string(),
                                                    ));
                                                    break;
                                                } else {
                                                    break;
                                                }
                                                continue;
                                            }
                                            _ => {}
                                        }
                                    }
                                    if let Some(message) = parse(&text) {
                                        let _ = events.send(message);
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
                        if auth_rejected {
                            break;
                        }
                        if was_connected {
                            was_connected = false;
                            let _ = status.send(Status::Disconnected);
                        }
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        if reconnect {
                            continue;
                        }
                    }
                    Err(_) => {
                        if was_connected {
                            was_connected = false;
                            let _ = status.send(Status::Disconnected);
                        }
                    }
                }
                attempts = attempts.saturating_add(1);
                let wait = Duration::from_secs((2u64 << attempts.min(5)).min(60));
                let deadline = Instant::now() + wait;
                while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        })
        .expect("no se pudo crear el hilo de PubSub")
}

fn parse(text: &str) -> Option<PubSubMessage> {
    let value: Value = serde_json::from_str(text).ok()?;
    if value.get("type").and_then(|kind| kind.as_str()) != Some("MESSAGE") {
        return None;
    }
    let topic = value.pointer("/data/topic")?.as_str()?.to_string();
    let raw = value.pointer("/data/message")?.as_str()?;
    let payload: Value = serde_json::from_str(raw).ok()?;
    Some(PubSubMessage { topic, payload })
}

/// Nonce hexadecimal de 8 bytes para la suscripción.
fn nonce() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos() as u64)
        .unwrap_or(0);
    format!("{:016x}", nanos)
}
