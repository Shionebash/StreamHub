//! Cliente de chat de Twitch basado en la librería `twitch_irc`.
//!
//! `twitch_irc` aporta el pool de conexiones, la reconexión automática, el
//! límite de mensajes y el parseo completo de IRCv3 (badges, emotes, color,
//! subs, raids, bits, mensajes del sistema...). Aquí se traduce ese modelo a
//! los eventos que consume la interfaz.

use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::time::{Duration, SystemTime};

use twitch_irc::login::StaticLoginCredentials;
use twitch_irc::message::ServerMessage;
use twitch_irc::transport::websocket::SecureWSTransport;
use twitch_irc::{ClientConfig, TwitchIRCClient};

/// Emote dentro de un mensaje (rango de **bytes** sobre `ChatEvent::text`).
#[derive(Clone, Debug)]
pub struct Emote {
    pub id: String,
    pub start: usize,
    pub end: usize,
}

/// Tipo de evento recibido del chat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Mensaje normal de un usuario.
    Message,
    /// Evento del sistema (suscripción, regalo, raid, bits).
    System,
    /// Aviso del servidor o de un moderador.
    Notice,
    /// Mensaje privado (whisper).
    Whisper,
    /// Acción `/me` del propio usuario.
    Action,
    /// Cambio de estado de la conexión.
    Status,
    /// Conjuntos de emotes disponibles para el usuario.
    EmoteSets,
    /// Un mensaje concreto se ha borrado (moderación).
    Delete,
    /// Se ha limpiado todo el chat del canal.
    Clear,
}

/// Evento de chat ya parseado.
#[derive(Clone, Debug)]
pub struct ChatEvent {
    pub kind: Kind,
    pub channel: String,
    pub login: String,
    pub display: String,
    /// Color del usuario en hexadecimal (`RRGGBB`).
    pub color: Option<String>,
    /// Badges en formato `tipo/versión`.
    pub badges: Vec<String>,
    pub text: String,
    pub emotes: Vec<Emote>,
    pub bits: u32,
    pub at: SystemTime,
    /// `Some(true/false)` en eventos de estado: conexión viva o caída.
    pub connected: Option<bool>,
    /// Conjuntos de emotes del usuario (solo en eventos `EmoteSets`).
    pub emote_sets: Vec<String>,
    /// ID único del mensaje (para borrarlo o responder).
    pub id: String,
    /// Mensaje al que responde: `(login, fragmento del texto)`.
    pub reply: Option<(String, String)>,
}

impl Default for ChatEvent {
    fn default() -> Self {
        Self {
            kind: Kind::Message,
            channel: String::new(),
            login: String::new(),
            display: String::new(),
            color: None,
            badges: Vec::new(),
            text: String::new(),
            emotes: Vec::new(),
            bits: 0,
            at: SystemTime::now(),
            connected: None,
            emote_sets: Vec::new(),
            id: String::new(),
            reply: None,
        }
    }
}

/// Órdenes que la interfaz envía al hilo del chat.
pub enum Command {
    Join(String),
    Part(String),
    Send {
        channel: String,
        text: String,
        /// ID del mensaje al que se responde (respuesta de Twitch).
        reply_to: Option<String>,
    },
    /// Mensaje privado a un usuario.
    Whisper { user: String, text: String },
    /// Cambia la identidad (token/login) y reconecta.
    Identity { token: String, login: String },
    /// Cierra el chat por completo (libera la conexión y la memoria).
    Shutdown,
}

/// Tipo de transporte usado (WebSocket con TLS del sistema).
type Client = TwitchIRCClient<SecureWSTransport, StaticLoginCredentials>;

/// Arranca el hilo del chat.
///
/// Devuelve el canal de órdenes; los eventos llegan por `events`.
pub fn spawn(
    token: String,
    login: String,
    events: Sender<ChatEvent>,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<Command>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("chat".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(_) => return,
            };
            runtime.block_on(async move {
                let mut token = token;
                let mut login = login;
                let mut joined: Vec<String> = Vec::new();
                loop {
                    // (Re)crea el cliente con la identidad actual.
                    let config = if token.trim().is_empty() {
                        ClientConfig::default()
                    } else {
                        ClientConfig::new_simple(StaticLoginCredentials::new(
                            login.trim().to_lowercase(),
                            Some(token.trim().to_string()),
                        ))
                    };
                    let (mut incoming, client) = Client::new(config);
                    let _ = events.send(status_event(true));

                    // Vuelca los mensajes entrantes al canal de la interfaz.
                    let forwarder = {
                        let events = events.clone();
                        tokio::spawn(async move {
                            while let Some(message) = incoming.recv().await {
                                if std::env::var("SH_DEBUG").is_ok() {
                                    use twitch_irc::message::AsRawIRC;
                                    let raw = message.source().as_raw_irc().to_string();
                                    let tail = &raw[raw.len().saturating_sub(90)..];
                                    eprintln!("[irc] {} | {}", message.source().command, tail);
                                }
                                for event in convert(message) {
                                    if events.send(event).is_err() {
                                        return;
                                    }
                                }
                            }
                        })
                    };

                    for channel in &joined {
                        let _ = client.join(channel.clone());
                    }

                    loop {
                        let command = commands.recv().await;
                        let Some(command) = command else { break };
                        match command {
                            Command::Join(channel) => {
                                let channel = channel.to_lowercase();
                                if !joined.contains(&channel) {
                                    joined.push(channel.clone());
                                }
                                let _ = client.join(channel);
                            }
                            Command::Part(channel) => {
                                let channel = channel.to_lowercase();
                                joined.retain(|name| name != &channel);
                                client.part(channel);
                            }
                            Command::Send {
                                channel,
                                text,
                                reply_to,
                            } => {
                                // `twitch_irc` no siempre devuelve la confirmación
                                // del envío, así que se limita el tiempo de espera
                                // para no bloquear el resto de órdenes.
                                let (channel, text, action) = match text.strip_prefix("/me ") {
                                    Some(rest) => (channel.to_lowercase(), rest.to_string(), true),
                                    None => (channel.to_lowercase(), text, false),
                                };
                                let result = match (&reply_to, action) {
                                    (Some(id), true) => tokio::time::timeout(
                                        Duration::from_secs(3),
                                        client.me_in_reply_to(&(channel.clone(), id.clone()), text),
                                    )
                                    .await
                                    .map(|result| result.map_err(|err| format!("{err:?}"))),
                                    (Some(id), false) => tokio::time::timeout(
                                        Duration::from_secs(3),
                                        client.say_in_reply_to(&(channel.clone(), id.clone()), text),
                                    )
                                    .await
                                    .map(|result| result.map_err(|err| format!("{err:?}"))),
                                    (None, true) => tokio::time::timeout(
                                        Duration::from_secs(3),
                                        client.me(channel.clone(), text),
                                    )
                                    .await
                                    .map(|result| result.map_err(|err| format!("{err:?}"))),
                                    (None, false) => tokio::time::timeout(
                                        Duration::from_secs(3),
                                        client.say(channel.clone(), text),
                                    )
                                    .await
                                    .map(|result| result.map_err(|err| format!("{err:?}"))),
                                };
                                if std::env::var("SH_DEBUG").is_ok() {
                                    eprintln!("[chat] envío: {result:?}");
                                }
                            }
                            Command::Whisper { user, text } => {
                                // Twitch sigue aceptando whispers por IRC en `#jtv`.
                                let result = tokio::time::timeout(
                                    Duration::from_secs(3),
                                    client.privmsg("jtv".to_string(), format!("/w {user} {text}")),
                                )
                                .await;
                                if std::env::var("SH_DEBUG").is_ok() {
                                    eprintln!("[chat] whisper: {result:?}");
                                }
                            }
                            Command::Identity {
                                token: new_token,
                                login: new_login,
                            } => {
                                token = new_token;
                                login = new_login;
                                // Se fuerza la reconexión con la nueva sesión.
                                forwarder.abort();
                                let _ = events.send(status_event(false));
                                break;
                            }
                            Command::Shutdown => {
                                let _ = events.send(status_event(false));
                                // Salir del bucle exterior: si solo se rompe el
                                // interior, se reconectaría y `join()` no
                                // terminaría nunca.
                                return;
                            }
                        }
                    }
                }
            });
        })
        .expect("no se pudo crear el hilo del chat")
}

/// Traduce un mensaje de `twitch_irc` a los eventos de la interfaz.
fn convert(message: ServerMessage) -> Vec<ChatEvent> {
    let now = SystemTime::now();
    match message {
        ServerMessage::Privmsg(msg) => {
            // Los rangos de `twitch_irc` están en caracteres, pero el texto se
            // corta por bytes. Se convierten una sola vez por mensaje.
            let mut offsets: Vec<usize> = msg.message_text.char_indices().map(|(i, _)| i).collect();
            offsets.push(msg.message_text.len());
            let emotes = msg
                .emotes
                .iter()
                .filter_map(|emote| {
                    let start = *offsets.get(emote.char_range.start)?;
                    let end = *offsets.get(emote.char_range.end)?;
                    Some(Emote {
                        id: emote.id.clone(),
                        start,
                        end,
                    })
                })
                .collect();
            let bits = msg.bits.unwrap_or(0) as u32;
            vec![ChatEvent {
                kind: if msg.is_action {
                    Kind::Action
                } else if bits > 0 {
                    Kind::System
                } else {
                    Kind::Message
                },
                channel: msg.channel_login.to_lowercase(),
                login: msg.sender.login.to_lowercase(),
                display: msg.sender.name.clone(),
                color: msg.name_color.map(|color| color.to_string()),
                badges: msg
                    .badges
                    .iter()
                    .map(|badge| format!("{}/{}", badge.name, badge.version))
                    .collect(),
                text: msg.message_text.clone(),
                emotes,
                bits,
                at: now,
                connected: None,
                emote_sets: Vec::new(),
                id: msg.message_id.clone(),
                reply: msg.reply_parent.as_ref().map(|parent| {
                    (
                        parent.reply_parent_user.login.to_lowercase(),
                        parent.message_text.chars().take(80).collect(),
                    )
                }),
            }]
        }
        ServerMessage::UserNotice(msg) => {
            let text = if msg.system_message.is_empty() {
                msg.event_id.clone()
            } else {
                msg.system_message.clone()
            };
            vec![ChatEvent {
                kind: Kind::System,
                channel: msg.channel_login.to_lowercase(),
                login: msg.sender.login.to_lowercase(),
                display: msg.sender.name.clone(),
                color: None,
                badges: msg
                    .badges
                    .iter()
                    .map(|badge| format!("{}/{}", badge.name, badge.version))
                    .collect(),
                text,
                emotes: Vec::new(),
                bits: 0,
                at: now,
                connected: None,
                emote_sets: Vec::new(),
                id: msg.message_id.clone(),
                reply: None,
            }]
        }
        ServerMessage::Notice(msg) => {
            let Some(channel) = msg.channel_login.clone() else {
                return Vec::new();
            };
            if channel.is_empty() {
                return Vec::new();
            }
            vec![ChatEvent {
                kind: Kind::Notice,
                channel: channel.to_lowercase(),
                login: String::new(),
                display: String::new(),
                color: None,
                badges: Vec::new(),
                text: msg.message_text.clone(),
                emotes: Vec::new(),
                bits: 0,
                at: now,
                connected: None,
                emote_sets: Vec::new(),
                id: String::new(),
                reply: None,
            }]
        }
        ServerMessage::Whisper(msg) => {
            vec![ChatEvent {
                kind: Kind::Whisper,
                channel: String::new(),
                login: msg.sender.login.to_lowercase(),
                display: msg.sender.name.clone(),
                color: msg.name_color.map(|color| color.to_string()),
                badges: msg
                    .badges
                    .iter()
                    .map(|badge| format!("{}/{}", badge.name, badge.version))
                    .collect(),
                text: msg.message_text.clone(),
                emotes: Vec::new(),
                bits: 0,
                at: now,
                connected: None,
                emote_sets: Vec::new(),
                id: String::new(),
                reply: None,
            }]
        }
        ServerMessage::GlobalUserState(msg) => {
            let mut sets: Vec<String> = msg.emote_sets.into_iter().collect();
            sets.sort();
            vec![ChatEvent {
                kind: Kind::EmoteSets,
                channel: String::new(),
                login: String::new(),
                display: String::new(),
                color: None,
                badges: Vec::new(),
                text: String::new(),
                emotes: Vec::new(),
                bits: 0,
                at: now,
                connected: None,
                emote_sets: sets,
                id: String::new(),
                reply: None,
            }]
        }
        ServerMessage::ClearMsg(msg) => {
            vec![ChatEvent {
                kind: Kind::Delete,
                channel: msg.channel_login.to_lowercase(),
                login: msg.sender_login.to_lowercase(),
                display: String::new(),
                color: None,
                badges: Vec::new(),
                text: msg.message_text.clone(),
                emotes: Vec::new(),
                bits: 0,
                at: now,
                connected: None,
                emote_sets: Vec::new(),
                id: msg.message_id.clone(),
                reply: None,
            }]
        }
        ServerMessage::ClearChat(msg) => {
            let (kind, text) = match &msg.action {
                twitch_irc::message::ClearChatAction::ChatCleared => {
                    (Kind::Clear, "El chat fue limpiado".to_string())
                }
                twitch_irc::message::ClearChatAction::UserBanned { user_login, .. } => (
                    Kind::System,
                    format!("{user_login} fue expulsado del chat"),
                ),
                twitch_irc::message::ClearChatAction::UserTimedOut {
                    user_login,
                    timeout_length,
                    ..
                } => (
                    Kind::System,
                    format!(
                        "{user_login} recibió un timeout de {} s",
                        timeout_length.as_secs()
                    ),
                ),
            };
            vec![ChatEvent {
                kind,
                channel: msg.channel_login.to_lowercase(),
                login: String::new(),
                display: String::new(),
                color: None,
                badges: Vec::new(),
                text,
                emotes: Vec::new(),
                bits: 0,
                at: now,
                connected: None,
                emote_sets: Vec::new(),
                id: String::new(),
                reply: None,
            }]
        }
        other => {
            // Twitch ya no envía el tag `badge-info` que la crate exige para
            // interpretar un PRIVMSG, así que many mensajes llegan como
            // `Generic`. Los interpretamos nosotros con las etiquetas crudas.
            use twitch_irc::message::AsRawIRC;
            let raw = other.source().as_raw_irc().to_string();
            parse_raw(&raw)
        }
    }
}

/// Parsea una línea IRCv3 con las etiquetas que envía Twitch hoy.
fn parse_raw(line: &str) -> Vec<ChatEvent> {
    let mut tags: HashMap<String, String> = HashMap::new();
    let mut rest = line.trim();
    if let Some(stripped) = rest.strip_prefix('@') {
        if let Some((tag_text, remainder)) = stripped.split_once(' ') {
            for tag in tag_text.split(';') {
                if let Some((key, value)) = tag.split_once('=') {
                    tags.insert(key.to_string(), unescape_tag(value));
                }
            }
            rest = remainder;
        }
    }
    // Prefijo (`:nick!user@host`), comando y parámetros según IRCv3.
    // Ojo: Twitch omite el `:` final cuando el mensaje no tiene espacios
    // (`PRIVMSG #canal KEKW`), así que no se puede buscar `" :"`.
    let prefix = match rest.strip_prefix(':') {
        Some(after) => match after.split_once(' ') {
            Some((prefix, remainder)) => {
                rest = remainder;
                prefix
            }
            None => {
                rest = "";
                after
            }
        },
        None => "",
    };
    let (command, after_command) = rest.split_once(' ').unwrap_or((rest, ""));
    let (target, tail) = after_command.split_once(' ').unwrap_or((after_command, ""));
    let command = command.to_uppercase();
    let target = target.to_string();
    let text = tail.strip_prefix(':').unwrap_or(tail).to_string();
    let nick = prefix.split('!').next().unwrap_or_default().to_string();
    let login = nick.to_lowercase();
    let channel = target.trim_start_matches('#').to_lowercase();
    let now = SystemTime::now();
    // La marca de tiempo del servidor permite ordenar el historial.
    let at = tags
        .get("tmi-sent-ts")
        .and_then(|value| value.parse::<u64>().ok())
        .and_then(|millis| SystemTime::UNIX_EPOCH.checked_add(Duration::from_millis(millis)))
        .unwrap_or(now);
    let display_name = tags
        .get("display-name")
        .filter(|name| !name.is_empty())
        .cloned()
        .unwrap_or_else(|| login.clone());
    let badges = tags
        .get("badges")
        .map(|value| {
            value
                .split(',')
                .filter(|badge| !badge.is_empty())
                .map(|badge| badge.to_string())
                .collect()
        })
        .unwrap_or_default();
    let id = tags.get("id").cloned().unwrap_or_default();

    match command.as_str() {
        "PRIVMSG" => {
            let bits = tags
                .get("bits")
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(0);
            let emotes = parse_emotes(tags.get("emotes").map(|s| s.as_str()), &text);
            // `/me` llega como un PRIVMSG con el prefijo \u{1}ACTION.
            let is_action = text.starts_with('\u{1}');
            let text = if is_action {
                text.trim_start_matches('\u{1}')
                    .trim_end_matches('\u{1}')
                    .trim_start_matches("ACTION ")
                    .to_string()
            } else {
                text
            };
            let reply = tags
                .get("reply-parent-user-login")
                .map(|parent| {
                    (
                        parent.to_lowercase(),
                        tags.get("reply-parent-msg-body")
                            .cloned()
                            .unwrap_or_default()
                            .chars()
                            .take(80)
                            .collect(),
                    )
                });
            vec![ChatEvent {
                kind: if is_action {
                    Kind::Action
                } else if bits > 0 {
                    Kind::System
                } else {
                    Kind::Message
                },
                channel,
                login: login.clone(),
                display: display_name,
                color: tags.get("color").map(|c| c.to_string()),
                badges,
                text,
                emotes,
                bits,
                at,
                connected: None,
                emote_sets: Vec::new(),
                id,
                reply,
            }]
        }
        "USERNOTICE" => {
            let mut text = text;
            if text.is_empty() {
                let msg_id = tags.get("msg-id").cloned().unwrap_or_default();
                text = match msg_id.as_str() {
                    "sub" | "resub" => {
                        let count = tags
                            .get("msg-param-sub-count")
                            .cloned()
                            .unwrap_or_else(|| "1".into());
                        format!("se suscribió ({count} mes/es)")
                    }
                    "subgift" | "anonsubgift" => {
                        let count = tags
                            .get("msg-param-sub-count")
                            .cloned()
                            .unwrap_or_else(|| "1".into());
                        format!("regaló {count} suscripción(es)")
                    }
                    "submysterygift" => {
                        let count = tags
                            .get("msg-param-mass-gift-count")
                            .cloned()
                            .unwrap_or_else(|| "1".into());
                        format!("regaló {count} suscripciones")
                    }
                    "raid" => {
                        let viewers = tags
                            .get("msg-param-viewerCount")
                            .cloned()
                            .unwrap_or_default();
                        format!("hizo raid con {viewers} espectadores")
                    }
                    _ => msg_id,
                };
            }
            vec![ChatEvent {
                kind: Kind::System,
                channel,
                login,
                display: display_name,
                color: tags.get("color").map(|c| c.to_string()),
                badges,
                text,
                emotes: Vec::new(),
                bits: 0,
                at,
                connected: None,
                emote_sets: Vec::new(),
                id: tags.get("id").cloned().unwrap_or_default(),
                reply: None,
            }]
        }
        // Un moderador borró un mensaje concreto.
        "CLEARMSG" => vec![ChatEvent {
            kind: Kind::Delete,
            channel,
            login: tags
                .get("login")
                .map(|login| login.to_lowercase())
                .unwrap_or(login),
            display: String::new(),
            color: None,
            badges: Vec::new(),
            text,
            emotes: Vec::new(),
            bits: 0,
            at,
            connected: None,
            emote_sets: Vec::new(),
            id: tags.get("target-msg-id").cloned().unwrap_or_default(),
            reply: None,
        }],
        // Se limpió todo el chat (o hubo un timeout sin `twitch_irc`).
        "CLEAR" => vec![ChatEvent {
            kind: Kind::Clear,
            channel,
            login: String::new(),
            display: String::new(),
            color: None,
            badges: Vec::new(),
            text: "El chat fue limpiado".to_string(),
            emotes: Vec::new(),
            bits: 0,
            at,
            connected: None,
            emote_sets: Vec::new(),
            id: String::new(),
            reply: None,
        }],
        _ => Vec::new(),
    }
}

/// Deshace los escapes de los valores de etiquetas IRCv3.
fn unescape_tag(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some(':') => out.push(';'),
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some('\\') => out.push('\\'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}


/// Convierte el tag `emotes` (índices en caracteres) a rangos de bytes.
fn parse_emotes(raw: Option<&str>, text: &str) -> Vec<Emote> {
    let Some(raw) = raw else { return Vec::new() };
    let mut out = Vec::new();
    let mut offsets: Vec<usize> = Vec::with_capacity(text.len() + 1);
    for (index, _) in text.char_indices() {
        offsets.push(index);
    }
    offsets.push(text.len());
    for group in raw.split('/') {
        let Some((id, ranges)) = group.split_once(':') else {
            continue;
        };
        if id.is_empty() {
            continue;
        }
        for range in ranges.split(',') {
            let Some((from, to)) = range.split_once('-') else {
                continue;
            };
            let (Ok(from), Ok(to)) = (from.parse::<usize>(), to.parse::<usize>()) else {
                continue;
            };
            let start = offsets.get(from).copied();
            let end = offsets.get(to + 1).copied();
            if let (Some(start), Some(end)) = (start, end) {
                out.push(Emote {
                    id: id.to_string(),
                    start,
                    end,
                });
            }
        }
    }
    out.sort_by_key(|emote| emote.start);
    out
}

/// Parsea varias líneas IRC (por ejemplo, el historial reciente).
pub fn parse_lines(lines: impl IntoIterator<Item = String>) -> Vec<ChatEvent> {
    let mut out = Vec::new();
    for line in lines {
        out.extend(parse_raw(&line));
    }
    out.retain(|event| {
        matches!(
            event.kind,
            Kind::Message | Kind::Action | Kind::System | Kind::Notice | Kind::Delete | Kind::Clear
        )
    });
    out
}

/// Descarga los últimos mensajes de un canal desde la API pública de
/// `recent-messages.robotty.de` (la misma que usa Chatterino).
pub fn recent_messages(channel: &str) -> Result<Vec<ChatEvent>, String> {
    static CLIENT: std::sync::OnceLock<reqwest::blocking::Client> = std::sync::OnceLock::new();
    let client = CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .user_agent("StreamHubNative/0.1 (Chatterino-compatible client)")
            .timeout(Duration::from_secs(8))
            .build()
            .unwrap_or_default()
    });
    let url = format!(
        "https://recent-messages.robotty.de/api/v2/recent-messages/{}?limit=100",
        channel.trim().to_lowercase()
    );
    let response = client
        .get(url)
        .send()
        .map_err(|err| format!("{err}"))?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    let json: serde_json::Value = response.json().map_err(|err| format!("{err}"))?;
    let lines = json
        .get("messages")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(|text| text.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(parse_lines(lines))
}

/// Evento de estado de la conexión.
fn status_event(connected: bool) -> ChatEvent {
    ChatEvent {
        kind: Kind::Status,
        channel: String::new(),
        login: String::new(),
        display: String::new(),
        color: None,
        badges: Vec::new(),
        text: String::new(),
        emotes: Vec::new(),
        bits: 0,
        at: SystemTime::now(),
        connected: Some(connected),
        emote_sets: Vec::new(),
        id: String::new(),
        reply: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_privmsg_like_twitch() {
        // La crate `twitch_irc` exige el tag `badge-info`, que Twitch ya no
        // envía: por eso interpretamos las etiquetas nosotros.
        let raw = "@emotes;color=#1E90FF;returning-chatter=0;turbo=0;mod=0;subscriber=0;badges=subscriber/42,alliance/1;flags;user-type;display-name=ChaoticCanadian;room-id=26301881;tmi-sent-ts=1794524269415;id=fb0d7340-4593-4a0f-b336-ac4d00947100;user-id=115994763;first-msg=0 :chaoticcanadian!chaoticcanadian@chaoticcanadian.tmi.twitch.tv PRIVMSG #sodapoppin :hola Kappa";
        let events = parse_raw(raw);
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.kind, Kind::Message);
        assert_eq!(event.channel, "sodapoppin");
        assert_eq!(event.display, "ChaoticCanadian");
        assert_eq!(event.text, "hola Kappa");
        assert_eq!(
            event.badges,
            vec!["subscriber/42".to_string(), "alliance/1".to_string()]
        );
    }

    #[test]
    fn parses_single_word_without_trailing_colon() {
        // Twitch omite el `:` cuando el mensaje no tiene espacios.
        let raw = "@display-name=Galaron1;id=abc :galaron1!galaron1@galaron1.tmi.twitch.tv PRIVMSG #sodapoppin Aware";
        let events = parse_raw(raw);
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.text, "Aware");
        assert_eq!(event.login, "galaron1");
        assert_eq!(event.channel, "sodapoppin");
        assert_eq!(event.id, "abc");
    }
}