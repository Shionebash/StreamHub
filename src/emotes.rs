//! Emotes de proveedores externos: BetterTTV y FrankerFaceZ.
//!
//! Misma API que usa Chatterino:
//! - BTTV: `api.betterttv.net/3/cached/emotes/global` y `.../users/twitch/{id}`.
//! - FFZ:  `api.frankerfacez.com/v1/set/global` y `.../v1/room/id/{id}`.
//!
//! Las imágenes se sirven en GIF (BTTV) y PNG/WebP (FFZ), formatos que el
//! decodificador de egui admite.

use std::time::Duration;

const BTTV_API: &str = "https://api.betterttv.net/3/cached";
const BTTV_CDN: &str = "https://cdn.betterttv.net/emote";
const FFZ_API: &str = "https://api.frankerfacez.com/v1";

/// Emote de un proveedor externo: nombre y URL de la imagen.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ExternalEmote {
    pub name: String,
    pub url: String,
}

fn client() -> &'static reqwest::blocking::Client {
    static CLIENT: std::sync::OnceLock<reqwest::blocking::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/125.0.0.0")
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default()
    })
}

fn get_json(url: &str) -> Result<serde_json::Value, String> {
    let response = client()
        .get(url)
        .send()
        .map_err(|err| format!("{url}: {err}"))?;
    if !response.status().is_success() {
        return Err(format!("{url} respondió {}", response.status()));
    }
    response.json().map_err(|err| format!("{url}: {err}"))
}

/// Emotes globales de BetterTTV.
pub fn bttv_global() -> Result<Vec<ExternalEmote>, String> {
    let value = get_json(&format!("{BTTV_API}/emotes/global"))?;
    Ok(parse_bttv(&value))
}

/// Emotes de BetterTTV de un canal (los del canal y los compartidos).
pub fn bttv_channel(channel_id: &str) -> Result<Vec<ExternalEmote>, String> {
    let value = get_json(&format!("{BTTV_API}/users/twitch/{channel_id}"))?;
    let mut out = Vec::new();
    for key in ["channelEmotes", "sharedEmotes"] {
        if let Some(list) = value.get(key) {
            out.extend(parse_bttv(list));
        }
    }
    Ok(out)
}

fn parse_bttv(value: &serde_json::Value) -> Vec<ExternalEmote> {
    let Some(list) = value.as_array() else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|emote| {
            let id = emote.get("id")?.as_str()?;
            let code = emote.get("code")?.as_str()?;
            Some(ExternalEmote {
                name: code.to_string(),
                // GIF: es el formato que egui decodifica de forma fiable.
                // Sin extensión: BTTV sirve PNG o WebP según el emote y
                // `/2x.gif` devuelve 404 en los estáticos antiguos.
                url: format!("{BTTV_CDN}/{id}/2x"),
            })
        })
        .collect()
}

/// Emotes globales de FrankerFaceZ.
pub fn ffz_global() -> Result<Vec<ExternalEmote>, String> {
    let value = get_json(&format!("{FFZ_API}/set/global"))?;
    let mut out = Vec::new();
    if let Some(sets) = value.get("sets").and_then(|v| v.as_object()) {
        for set in sets.values() {
            out.extend(parse_ffz(set));
        }
    }
    Ok(out)
}

/// Emotes de FrankerFaceZ de un canal.
pub fn ffz_channel(channel_id: &str) -> Result<Vec<ExternalEmote>, String> {
    let value = get_json(&format!("{FFZ_API}/room/id/{channel_id}"))?;
    let mut out = Vec::new();
    if let Some(sets) = value.get("sets").and_then(|v| v.as_object()) {
        for set in sets.values() {
            out.extend(parse_ffz(set));
        }
    }
    Ok(out)
}

/// ¿Son la misma lista? Sirve para no reescribir la caché sin cambios.
pub fn same_list(a: &[ExternalEmote], b: &[ExternalEmote]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| x.name == y.name && x.url == y.url)
}

/// FFZ devuelve URLs protocol-relative (`//cdn.frankerfacez.com/...`); el
/// cargador de imágenes necesita el esquema completo.
fn absolute_url(url: &str) -> String {
    if url.starts_with("//") {
        format!("https:{url}")
    } else {
        url.to_string()
    }
}

fn parse_ffz(set: &serde_json::Value) -> Vec<ExternalEmote> {    let Some(list) = set.get("emoticons").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|emote| {
            let name = emote.get("name")?.as_str()?;
            let urls = emote.get("urls")?;
            let url = urls
                .get("2")
                .or_else(|| urls.get("1"))
                .and_then(|v| v.as_str())?;
            Some(ExternalEmote {
                name: name.to_string(),
                url: absolute_url(url),
            })
        })
        .collect()
}
