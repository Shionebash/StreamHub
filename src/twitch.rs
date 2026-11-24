#![allow(dead_code)]

use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

const HELIX: &str = "https://api.twitch.tv/helix";

#[derive(Clone, Debug, Default, Deserialize)]
pub struct StreamInfo {
    #[serde(default)]
    pub user_login: String,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub game_name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub viewer_count: u64,
    #[serde(default)]
    pub thumbnail_url: String,
    #[serde(default)]
    pub started_at: String,
    #[serde(rename = "type", default)]
    pub kind: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct UserInfo {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub login: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub profile_image_url: String,
}

/// Emote de un conjunto según la API Helix de chat.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct HelixEmote {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "emote_set_id", default)]
    pub emote_set_id: String,
    #[serde(rename = "owner_id", default)]
    pub owner_id: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct FollowedChannel {
    #[serde(default)]
    pub broadcaster_id: String,
    #[serde(default)]
    pub broadcaster_login: String,
    #[serde(default)]
    pub broadcaster_name: String,
    #[serde(default)]
    pub followed_at: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Game {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub box_art_url: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SearchResult {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub broadcaster_login: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub game_name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub is_live: bool,
    #[serde(default)]
    pub thumbnail_url: String,
    #[serde(default)]
    pub started_at: String,
    #[serde(default)]
    pub viewer_count: u64,
}

#[derive(Default, Deserialize)]
struct Pagination {
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Deserialize)]
struct Envelope<T> {
    #[serde(default = "Vec::new")]
    data: Vec<T>,
    #[serde(default)]
    pagination: Pagination,
}

#[derive(Clone, Debug, Default)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub cursor: Option<String>,
}

pub struct TwitchClient {
    http: Client,
    client_id: String,
    token: String,
}

impl TwitchClient {
    pub fn new(client_id: &str, token: &str) -> Result<Self> {
        if client_id.trim().is_empty() {
            bail!("falta el Client ID de Twitch");
        }
        let http = Client::builder()
            .user_agent(format!("StreamHub/{}", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(10))
            .build()
            .context("no se pudo crear el cliente HTTP")?;
        Ok(Self {
            http,
            client_id: client_id.trim().to_string(),
            token: token.trim().to_string(),
        })
    }

    fn get<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        let mut req = self.http.get(url).header("Client-Id", &self.client_id);
        if !self.token.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", self.token));
        }
        let resp = req.send().with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            bail!("Twitch respondió {status}: {}", body.chars().take(200).collect::<String>());
        }
        resp.json::<T>().context("respuesta JSON inválida de Twitch")
    }

    pub fn get_streams(&self, logins: &[String]) -> Result<HashMap<String, StreamInfo>> {
        let mut out = HashMap::new();
        for chunk in logins.chunks(100) {
            let mut query: Vec<(&str, &str)> = vec![("first", "100")];
            for login in chunk {
                query.push(("user_login", login.as_str()));
            }
            let env: Envelope<StreamInfo> = self.get(&build_url("/streams", &query))?;
            for mut stream in env.data {
                stream.title = clean_text(&stream.title);
                stream.game_name = clean_text(&stream.game_name);
                stream.user_name = clean_text(&stream.user_name);
                out.insert(stream.user_login.to_lowercase(), stream);
            }
        }
        Ok(out)
    }

    pub fn get_users(&self, logins: &[String]) -> Result<HashMap<String, UserInfo>> {
        let mut out = HashMap::new();
        for chunk in logins.chunks(100) {
            let mut query: Vec<(&str, &str)> = vec![("first", "100")];
            for login in chunk {
                query.push(("login", login.as_str()));
            }
            let env: Envelope<UserInfo> = self.get(&build_url("/users", &query))?;
            for mut user in env.data {
                user.display_name = clean_text(&user.display_name);
                out.insert(user.login.to_lowercase(), user);
            }
        }
        Ok(out)
    }

    pub fn get_users_by_ids(&self, ids: &[String]) -> Result<HashMap<String, UserInfo>> {
        let mut out = HashMap::new();
        for chunk in ids.chunks(100) {
            let mut query: Vec<(&str, &str)> = vec![("first", "100")];
            for id in chunk {
                query.push(("id", id.as_str()));
            }
            let env: Envelope<UserInfo> = self.get(&build_url("/users", &query))?;
            for mut user in env.data {
                user.display_name = clean_text(&user.display_name);
                out.insert(user.login.to_lowercase(), user);
            }
        }
        Ok(out)
    }

    /// Emotes de un conjunto por Helix (la vía que usa la web cuando la
    /// consulta GQL clásica de `emoteSet` ya no devuelve datos).
    pub fn get_emote_set(&self, set_id: &str) -> Result<Vec<HelixEmote>> {
        let url = build_url("/chat/emotes/set", &[("emote_set_id", set_id)]);
        let env: Envelope<HelixEmote> = self.get(&url)?;
        Ok(env.data)
    }

    pub fn get_me(&self) -> Result<Option<UserInfo>> {
        let env: Envelope<UserInfo> = self.get(&format!("{HELIX}/users"))?;
        Ok(env.data.into_iter().next())
    }

    pub fn get_followed(&self, user_id: &str) -> Result<Vec<FollowedChannel>> {
        let url = format!(
            "{HELIX}/channels/followed?user_id={}&first=100",
            urlencode(user_id)
        );
        let env: Envelope<FollowedChannel> = self.get(&url)?;
        let mut channels = env.data;
        for channel in &mut channels {
            channel.broadcaster_name = clean_text(&channel.broadcaster_name);
        }
        Ok(channels)
    }

    pub fn get_top_games(&self, after: Option<&str>) -> Result<Page<Game>> {
        let mut query = vec![("first", "30")];
        if let Some(cursor) = after.filter(|c| !c.is_empty()) {
            query.push(("after", cursor));
        }
        let env: Envelope<Game> = self.get(&build_url("/games/top", &query))?;
        Ok(Page {
            items: env.data,
            cursor: env.pagination.cursor.filter(|c| !c.is_empty()),
        })
    }

    pub fn search_channels(&self, query: &str) -> Result<Vec<SearchResult>> {
        let url = format!(
            "{HELIX}/search/channels?query={}&first=30&live_only=true",
            urlencode(query)
        );
        let mut env: Envelope<SearchResult> = self.get(&url)?;

        // `/search/channels` devuelve la foto de perfil en `thumbnail_url`.
        // Para los canales en vivo se sustituye por la preview real del stream
        // y se añaden los espectadores consultando `/streams`.
        let logins: Vec<String> = env
            .data
            .iter()
            .filter(|result| result.is_live && !result.broadcaster_login.is_empty())
            .map(|result| result.broadcaster_login.to_lowercase())
            .collect();
        if !logins.is_empty() {
            let streams = self.get_streams(&logins).unwrap_or_default();
            for result in &mut env.data {
                if !result.is_live {
                    continue;
                }
                let login = result.broadcaster_login.to_lowercase();
                match streams.get(&login) {
                    Some(stream) => {
                        result.viewer_count = stream.viewer_count;
                        if !stream.thumbnail_url.is_empty() {
                            result.thumbnail_url = stream.thumbnail_url.clone();
                        }
                        if result.game_name.is_empty() {
                            result.game_name = stream.game_name.clone();
                        }
                        if result.title.is_empty() {
                            result.title = stream.title.clone();
                        }
                    }
                    None => {
                        result.thumbnail_url = format!(
                            "https://static-cdn.jtvnw.net/previews-ttv/live_user_{login}-{{width}}x{{height}}.jpg"
                        );
                    }
                }
            }
        }
        for result in &mut env.data {
            result.display_name = clean_text(&result.display_name);
            result.title = clean_text(&result.title);
            result.game_name = clean_text(&result.game_name);
        }
        Ok(env.data)
    }

    pub fn get_streams_by_game(
        &self,
        game_id: &str,
        after: Option<&str>,
    ) -> Result<Page<StreamInfo>> {
        let mut query = vec![("game_id", game_id), ("first", "24")];
        if let Some(cursor) = after.filter(|c| !c.is_empty()) {
            query.push(("after", cursor));
        }
        let mut env: Envelope<StreamInfo> = self.get(&build_url("/streams", &query))?;
        for stream in &mut env.data {
            stream.title = clean_text(&stream.title);
            stream.game_name = clean_text(&stream.game_name);
            stream.user_name = clean_text(&stream.user_name);
        }
        Ok(Page {
            items: env.data,
            cursor: env.pagination.cursor.filter(|c| !c.is_empty()),
        })
    }
}

fn build_url(path: &str, query: &[(&str, &str)]) -> String {
    let mut url = format!("{HELIX}{path}");
    let mut first = true;
    for (key, value) in query {
        url.push(if first { '?' } else { '&' });
        first = false;
        url.push_str(&urlencode(key));
        url.push('=');
        url.push_str(&urlencode(value));
    }
    url
}

/// Elimina emoji y selectores de variación: egui no tiene fuente de emoji y
/// los muestra como cajas. Se aplica a textos que vienen de Twitch.
pub fn clean_text(value: &str) -> String {
    let filtered: String = value
        .chars()
        .filter(|ch| {
            let cp = *ch as u32;
            !matches!(
                cp,
                0x200D                 // zero-width joiner
                | 0x20E3               // combining enclosing keycap
                | 0x2600..=0x27BF      // símbolos misceláneos y dingbats
                | 0x2B00..=0x2BFF      // flechas y símbolos
                | 0xFE0F               // variation selector-16
                | 0x1F000..=0x1FAFF    // emoji
                | 0xE0020..=0xE007F    // etiquetas
            )
        })
        .collect();
    filtered.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
