//! Integración con 7TV: emotes de 7TV por canal y del usuario.
//!
//! API pública de 7TV (misma que usa Chatterino):
//! - `GET /v3/emote-sets/global` → emotes globales.
//! - `GET /v3/users/twitch/<id>`  → usuario de Twitch con su conjunto de emotes.
//!
//! Las imágenes se sirven desde `cdn.7tv.app`.

use std::time::Duration;

/// Un emote de 7TV.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SevenEmote {
    pub id: String,
    pub name: String,
    /// URL de la imagen (2x).
    pub url: String,
}

const API: &str = "https://7tv.io/v3";

fn client() -> &'static reqwest::blocking::Client {
    static CLIENT: std::sync::OnceLock<reqwest::blocking::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36")
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default()
    })
}

/// Descarga y normaliza un conjunto de emotes de 7TV.
pub fn parse_emotes(value: &serde_json::Value) -> Vec<SevenEmote> {
    let mut out = Vec::new();
    let Some(emotes) = value.get("emotes").and_then(|v| v.as_array()) else {
        return out;
    };
    for emote in emotes {
        let Some(id) = emote.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(name) = emote.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        // `data.host.url` es el prefijo; si no está, se construye el estándar.
        let host = emote
            .pointer("/data/host/url")
            .and_then(|v| v.as_str())
            .map(|url| {
                if let Some(rest) = url.strip_prefix("//") {
                    format!("https://{rest}")
                } else {
                    url.to_string()
                }
            })
            .unwrap_or_else(|| format!("https://cdn.7tv.app/emote/{id}"));
        // Se prefiere WebP: 7TV no genera GIF para todos los emotes y los
        // antiguos `1x.gif` devuelven 404. Nuestro decodificador soporta
        // WebP animado, así que la animación se conserva.
        let file = emote
            .pointer("/data/host/files")
            .and_then(|v| v.as_array())
            .and_then(|files| {
                let has = |wanted: &str| {
                    files
                        .iter()
                        .any(|file| file.get("name").and_then(|n| n.as_str()) == Some(wanted))
                };
                ["2x.webp", "2x.gif", "1x.webp", "1x.gif"]
                    .into_iter()
                    .find(|wanted| has(wanted))
            })
            .map(|name| name.to_string())
            .unwrap_or_else(|| "2x.webp".to_string());
        out.push(SevenEmote {
            id: id.to_string(),
            name: name.to_string(),
            url: format!("{host}/{file}"),
        });
    }
    out
}

/// Emotes globales de 7TV.
pub fn global_emotes() -> Result<Vec<SevenEmote>, String> {
    let response = client()
        .get(format!("{API}/emote-sets/global"))
        .send()
        .map_err(|err| format!("7TV: {err}"))?;
    if !response.status().is_success() {
        return Err(format!("7TV respondió {}", response.status()));
    }
    let value: serde_json::Value = response
        .json()
        .map_err(|err| format!("7TV: {err}"))?;
    Ok(parse_emotes(&value))
}

/// Emotes de un usuario de Twitch (su conjunto propio en 7TV).
pub fn user_emotes(twitch_id: &str) -> Result<Vec<SevenEmote>, String> {
    let response = client()
        .get(format!("{API}/users/twitch/{twitch_id}"))
        .send()
        .map_err(|err| format!("7TV: {err}"))?;
    if !response.status().is_success() {
        return Err(format!("7TV respondió {}", response.status()));
    }
    let value: serde_json::Value = response
        .json()
        .map_err(|err| format!("7TV: {err}"))?;
    let emotes = value
        .get("emote_set")
        .map(parse_emotes)
        .unwrap_or_default();
    Ok(emotes)
}

/// Emotes de un conjunto concreto.
#[allow(dead_code)]
pub fn emote_set(set_id: &str) -> Result<Vec<SevenEmote>, String> {
    let response = client()
        .get(format!("{API}/emote-sets/{set_id}"))
        .send()
        .map_err(|err| format!("7TV: {err}"))?;
    if !response.status().is_success() {
        return Err(format!("7TV respondió {}", response.status()));
    }
    let value: serde_json::Value = response
        .json()
        .map_err(|err| format!("7TV: {err}"))?;
    Ok(parse_emotes(&value))
}

/// ¿Son la misma lista? Sirve para no reescribir la caché sin cambios.
pub fn same_emotes(a: &[SevenEmote], b: &[SevenEmote]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| x.id == y.id && x.name == y.name && x.url == y.url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_7tv_emotes() {
        let emotes = global_emotes().expect("emotes globales");
        assert!(!emotes.is_empty());
        let emote = &emotes[0];
        let response = client()
            .get(&emote.url)
            .send()
            .expect("descarga");
        let bytes = response.bytes().expect("bytes");
        let decoded = image::load_from_memory(&bytes);
        println!("{} -> {:?}", emote.url, decoded.as_ref().map(|i| i.to_rgba8().dimensions()));
        assert!(decoded.is_ok(), "no se pudo decodificar el emote");
    }
}
