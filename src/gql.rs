//! Cliente GraphQL de Twitch: canje de puntos de canal y consulta de saldo.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

const GQL_URL: &str = "https://gql.twitch.tv/gql";
const SPADE_URL: &str = "https://spade.twitch.tv/track";
/// Client ID público de la web de Twitch. Necesario para que GQL acepte el
/// token del usuario (igual que hace la versión Python).
const WEB_CLIENT_ID: &str = "kimne78kx3ncx6brgo4mv6wki5h1ko";
const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36";

static TX_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Live stream metadata required for the `minute-watched` Spade heartbeat.
#[derive(Clone, Debug)]
pub struct StreamWatchInfo {
    pub channel_id: String,
    pub broadcast_id: String,
    pub game_id: String,
    pub game_name: String,
}

/// Custom channel points reward defined by a broadcaster.
#[derive(Clone, Debug)]
pub struct CustomReward {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub cost: u64,
    pub background_color: String,
    pub is_enabled: bool,
    pub is_paused: bool,
    pub is_in_stock: bool,
    pub is_user_input_required: bool,
    pub cooldown_expires_at: Option<String>,
}

/// Channel points state and available custom rewards for a channel.
#[derive(Clone, Debug, Default)]
pub struct ChannelPointsContext {
    pub channel_id: String,
    pub points_name: String,
    pub is_enabled: bool,
    pub balance: Option<u64>,
    pub available_claim_id: Option<String>,
    pub rewards: Vec<CustomReward>,
}

pub struct GqlClient {
    http: reqwest::blocking::Client,
    token: String,
}

/// Conjunto de emotes de Twitch y el canal que lo posee.
#[derive(Clone, Debug)]
pub struct EmoteSet {
    /// Identificador del conjunto.
    pub id: String,
    /// Login del canal dueño; vacío en los conjuntos globales.
    pub owner: String,
    /// Pares `(id, nombre)`.
    pub emotes: Vec<(String, String)>,
}

impl EmoteSet {
    fn parse(value: &serde_json::Value) -> Self {
        let owner = value
            .pointer("/owner/login")
            .and_then(|login| login.as_str())
            .unwrap_or_default()
            .to_lowercase();
        let emotes = value
            .get("emotes")
            .and_then(|emotes| emotes.as_array())
            .map(|emotes| {
                emotes
                    .iter()
                    .filter_map(|emote| {
                        let id = emote.get("id")?.as_str()?.to_string();
                        let text = emote.get("text")?.as_str()?.to_string();
                        Some((id, text))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            id: String::new(),
            owner,
            emotes,
        }
    }
}

impl GqlClient {
    pub fn new(token: &str) -> Result<Self, String> {
        let http = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|err| err.to_string())?;
        Ok(Self {
            http,
            token: token.trim().to_string(),
        })
    }

    fn post(&self, payload: serde_json::Value) -> Result<serde_json::Value, String> {
        let response = self
            .http
            .post(GQL_URL)
            .header("Client-ID", WEB_CLIENT_ID)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .map_err(|err| err.to_string())?;
        let status = response.status();
        let text = response.text().map_err(|err| err.to_string())?;
        if !status.is_success() {
            let short: String = text.chars().take(180).collect();
            return Err(format!("GQL respondió {status}: {short}"));
        }
        serde_json::from_str(&text).map_err(|err| err.to_string())
    }

    /// Canjea un cofre de puntos de canal.
    ///
    /// Devuelve el saldo nuevo, o `-1` si Twitch dice que ya estaba reclamado.
    pub fn claim_points(&self, channel_id: &str, claim_id: &str) -> Result<i64, String> {
        let payload = serde_json::json!({
            "query": "mutation ClaimCommunityPoints($input: ClaimCommunityPointsInput!) { claimCommunityPoints(input: $input) { currentPoints claim { id } } }",
            "variables": {"input": {"channelID": channel_id, "claimID": claim_id}}
        });
        let response = self.post(payload)?;
        if let Some(errors) = response.get("errors").and_then(|errors| errors.as_array()) {
            if !errors.is_empty() && response.get("data").is_none() {
                let message = errors
                    .iter()
                    .filter_map(|error| error.get("message").and_then(|m| m.as_str()))
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(format!("GQL: {message}"));
            }
        }
        match response.pointer("/data/claimCommunityPoints") {
            Some(serde_json::Value::Null) => Ok(-1),
            Some(value) => Ok(value
                .get("currentPoints")
                .and_then(|points| points.as_i64())
                .unwrap_or(-1)),
            None => Err("respuesta GQL sin datos".to_string()),
        }
    }

    /// Emotes de varios conjuntos en una sola consulta por lotes.
    ///
    /// Twitch devuelve el canal dueño de cada conjunto, que es lo que permite
    /// separar los emotes de suscripción por canal.
    ///
    /// Devuelve `(conjuntos, ids_fallidos)`: Twitch está retirando la consulta
    /// GQL de `emoteSet` y cada vez más conjuntos llegan vacíos o nulos; el
    /// llamador debe rescatar los fallidos por Helix.
    pub fn emote_sets(&self, ids: &[String]) -> (Vec<EmoteSet>, Vec<String>) {
        let ids: Vec<String> = ids
            .iter()
            .map(|id| {
                id.chars()
                    .filter(|c| c.is_ascii_alphanumeric())
                    .collect::<String>()
            })
            .filter(|id| !id.is_empty())
            .collect();
        let mut out = Vec::new();
        let mut failed = Vec::new();
        // GQL limita los alias de primer nivel a 15 por consulta.
        for chunk in ids.chunks(15) {
            let mut fields = String::new();
            for (index, id) in chunk.iter().enumerate() {
                fields.push_str(&format!(
                    "s{index}: emoteSet(id: \"{id}\") {{ owner {{ login }} emotes {{ id text }} }} "
                ));
            }
            let query = format!("query {{ {fields} }}");
            match self.post(serde_json::json!({ "query": query })) {
                Ok(response) => {
                    for (index, set_id) in chunk.iter().enumerate() {
                        match response.pointer(&format!("/data/s{index}")) {
                            Some(value) if !value.is_null() => {
                                let mut set = EmoteSet::parse(value);
                                set.id = set_id.clone();
                                if !set.emotes.is_empty() {
                                    out.push(set);
                                } else {
                                    failed.push(set_id.clone());
                                }
                            }
                            // Conjunto nulo o ausente: Helix lo rescatará.
                            _ => failed.push(chunk[index].clone()),
                        }
                    }
                }
                // Si el lote entero falla (red o GQL), no se pierden los 15:
                // se marcan como fallidos y Helix los recupera uno a uno.
                Err(_) => failed.extend(chunk.iter().cloned()),
            }
        }
        (out, failed)
    }

    /// Saldo de puntos de un canal. Solo funciona con el Client-ID web.
    pub fn points_balance(&self, login: &str) -> Result<Option<i64>, String> {
        let payload = serde_json::json!({
            "query": "query ChannelPoints($login: String!) { user(login: $login) { channel { self { communityPoints { balance } } } } }",
            "variables": {"login": login}
        });
        let response = self.post(payload)?;
        Ok(response
            .pointer("/data/user/channel/self/communityPoints/balance")
            .and_then(|balance| balance.as_i64()))
    }

    /// Fetches channel points balance, pending bonus claim, and custom rewards.
    pub fn channel_points_context(&self, login: &str) -> Result<ChannelPointsContext, String> {
        let payload = serde_json::json!({
            "query": "query ChannelPointsContext($login: String!) { user(login: $login) { id channel { id self { communityPoints { balance availableClaim { id } } } communityPointsSettings { name isEnabled customRewards { id title prompt cost backgroundColor isEnabled isPaused isInStock isUserInputRequired cooldownExpiresAt } } } } }",
            "variables": {"login": login}
        });
        let response = self.post(payload)?;
        let Some(user) = response.pointer("/data/user") else {
            return Err("canal no encontrado en GQL".to_string());
        };
        if user.is_null() {
            return Err("canal no encontrado en GQL".to_string());
        }
        let channel_id = user
            .pointer("/channel/id")
            .or_else(|| user.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let balance = user
            .pointer("/channel/self/communityPoints/balance")
            .and_then(|v| v.as_u64());
        let available_claim_id = user
            .pointer("/channel/self/communityPoints/availableClaim/id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let settings = user.pointer("/channel/communityPointsSettings");
        let points_name = settings
            .and_then(|s| s.get("name"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("Puntos del canal")
            .to_string();
        let is_enabled = settings
            .and_then(|s| s.get("isEnabled"))
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let mut rewards: Vec<CustomReward> = settings
            .and_then(|s| s.get("customRewards"))
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        let id = item.get("id")?.as_str()?.to_string();
                        let title = item
                            .get("title")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Recompensa")
                            .to_string();
                        let prompt = item
                            .get("prompt")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let cost = item.get("cost").and_then(|v| v.as_u64()).unwrap_or(0);
                        let background_color = item
                            .get("backgroundColor")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let is_enabled = item
                            .get("isEnabled")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(true);
                        let is_paused = item
                            .get("isPaused")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let is_in_stock = item
                            .get("isInStock")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(true);
                        let is_user_input_required = item
                            .get("isUserInputRequired")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let cooldown_expires_at = item
                            .get("cooldownExpiresAt")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .map(|s| s.to_string());
                        Some(CustomReward {
                            id,
                            title,
                            prompt,
                            cost,
                            background_color,
                            is_enabled,
                            is_paused,
                            is_in_stock,
                            is_user_input_required,
                            cooldown_expires_at,
                        })
                    })
                    .filter(|reward| reward.is_enabled)
                    .collect()
            })
            .unwrap_or_default();
        rewards.sort_by_key(|r| r.cost);

        Ok(ChannelPointsContext {
            channel_id,
            points_name,
            is_enabled,
            balance,
            available_claim_id,
            rewards,
        })
    }

    /// Redeems a custom channel points reward via GQL.
    pub fn redeem_custom_reward(
        &self,
        channel_id: &str,
        reward: &CustomReward,
        text_input: Option<&str>,
    ) -> Result<String, String> {
        let mut input = serde_json::json!({
            "channelID": channel_id,
            "rewardID": reward.id,
            "title": reward.title,
            "cost": reward.cost,
            "transactionID": transaction_id(),
        });
        if !reward.prompt.is_empty() {
            input["prompt"] = serde_json::Value::String(reward.prompt.clone());
        }
        if let Some(text) = text_input {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                input["textInput"] = serde_json::Value::String(trimmed.to_string());
            }
        }
        let payload = serde_json::json!({
            "query": "mutation RedeemCustomReward($input: RedeemCommunityPointsCustomRewardInput!) { redeemCommunityPointsCustomReward(input: $input) { redemption { id status } error { code } } }",
            "variables": { "input": input }
        });
        let response = self.post(payload)?;
        if let Some(code) = response
            .pointer("/data/redeemCommunityPointsCustomReward/error/code")
            .and_then(|v| v.as_str())
        {
            return Err(humanize_redeem_error(code));
        }
        if let Some(errors) = response.get("errors").and_then(|v| v.as_array()) {
            if !errors.is_empty() {
                let message = errors
                    .iter()
                    .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
                    .collect::<Vec<_>>()
                    .join("; ");
                if message.to_lowercase().contains("integrity") {
                    return Err("Twitch requiere verificación de integridad web para canjear esta recompensa (usa 'Abrir en Twitch')".to_string());
                }
                return Err(format!("GQL: {message}"));
            }
        }
        let id = response
            .pointer("/data/redeemCommunityPointsCustomReward/redemption/id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if id.is_empty() && response.pointer("/data/redeemCommunityPointsCustomReward").is_none() {
            return Err("Twitch no devolvió confirmación del canje".to_string());
        }
        Ok(id)
    }

    /// Fetches live broadcast metadata needed for the `minute-watched` beacon.
    pub fn stream_watch_info(&self, login: &str) -> Result<Option<StreamWatchInfo>, String> {
        let payload = serde_json::json!({
            "query": "query StreamWatchInfo($login: String!) { user(login: $login) { id stream { id game { id name } } } }",
            "variables": {"login": login}
        });
        let response = self.post(payload)?;
        let Some(user) = response.pointer("/data/user") else {
            return Ok(None);
        };
        let Some(stream) = user.get("stream").filter(|s| !s.is_null()) else {
            return Ok(None);
        };
        let channel_id = user
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let broadcast_id = stream
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if channel_id.is_empty() || broadcast_id.is_empty() {
            return Ok(None);
        }
        let game_id = stream
            .pointer("/game/id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let game_name = stream
            .pointer("/game/name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        Ok(Some(StreamWatchInfo {
            channel_id,
            broadcast_id,
            game_id,
            game_name,
        }))
    }

    /// Sends a `minute-watched` heartbeat to Twitch's Spade endpoint so watch
    /// time accumulates channel points and drops automatically.
    pub fn send_minute_watched(
        &self,
        info: &StreamWatchInfo,
        user_id: &str,
    ) -> Result<bool, String> {
        let payload = serde_json::json!([{
            "event": "minute-watched",
            "properties": {
                "broadcast_id": info.broadcast_id,
                "channel_id": info.channel_id,
                "player": "site",
                "user_id": user_id,
                "game": info.game_id,
                "live": true,
            }
        }]);
        let encoded = BASE64.encode(payload.to_string().as_bytes());
        let encoded_form = encode_form_component(&encoded);
        let body = format!("data={encoded_form}");
        let response = self
            .http
            .post(SPADE_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Referer", "https://www.twitch.tv/")
            .header("Origin", "https://www.twitch.tv")
            .body(body)
            .send()
            .map_err(|err| err.to_string())?;
        let status = response.status();
        Ok(status.as_u16() == 204 || status.is_success())
    }
}

fn encode_form_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 16);
    for ch in input.chars() {
        match ch {
            '+' => out.push_str("%2B"),
            '/' => out.push_str("%2F"),
            '=' => out.push_str("%3D"),
            _ => out.push(ch),
        }
    }
    out
}

fn transaction_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = TX_COUNTER.fetch_add(1, Ordering::Relaxed) as u128;
    let pid = std::process::id() as u128;
    let mixed = nanos ^ (seq.wrapping_mul(0x9E37_79B9_7F4A_7C15)) ^ (pid << 64);
    format!("{mixed:032x}")
}

fn humanize_redeem_error(code: &str) -> String {
    match code {
        "NOT_ENOUGH_BALANCE" => "No tienes suficientes puntos del canal".to_string(),
        "REWARD_PAUSED" => "Esta recompensa está pausada por el streamer".to_string(),
        "OUT_OF_STOCK" | "MAX_PER_STREAM_REACHED" => {
            "Esta recompensa alcanzó el límite de canjes del directo".to_string()
        }
        "MAX_PER_USER_PER_STREAM_REACHED" => {
            "Ya alcanzaste tu límite de canjes para esta recompensa".to_string()
        }
        "ON_COOLDOWN" | "COOLDOWN" => "Esta recompensa está en enfriamiento".to_string(),
        "TEXT_INPUT_REQUIRED" => "Esta recompensa requiere escribir un mensaje".to_string(),
        "SUBSCRIBER_ONLY" => "Esta recompensa es solo para suscriptores".to_string(),
        "FAILED_INTEGRITY_CHECK" | "INTEGRITY" => {
            "Twitch requiere verificación web para este canje (usa 'Abrir en Twitch')".to_string()
        }
        other => format!("No se pudo canjear ({other})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emote_sets_are_listed() {
        let token = crate::config::load_token().unwrap_or_default();
        let client = GqlClient::new(&token).unwrap();
        let (sets, failed) = client
            .emote_sets(&["0".to_string(), "301593312".to_string()]);
        for set in &sets {
            println!("{}: {} emotes", set.owner, set.emotes.len());
        }
        if !failed.is_empty() {
            println!("conjuntos sin datos en GQL: {failed:?}");
        }
        assert!(!sets.is_empty(), "no se listaron emotes");
        assert!(
            sets.iter().any(|set| !set.owner.is_empty()),
            "ningún conjunto trajo dueño"
        );
    }
}
