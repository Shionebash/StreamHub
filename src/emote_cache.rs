//! Caché en disco de emotes por canal.
//!
//! Cada canal tiene su propia carpeta `emotes/<canal>/` con:
//!
//! - `listas/{origen}.json`: la última lista descargada (7TV/BTTV/FFZ) y la
//!   marca de tiempo de la última comprobación.
//! - `img/<hash>.<ext>`: las imágenes de los emotes vistos en ese canal.
//!
//! Al abrir un canal se enseña la caché al instante y, si ha pasado el tiempo
//! de revalidación, se preguntan los cambios a los proveedores y se reescribe
//! la lista solo si ha variado. Así, volver al mismo directo no descarga nada.
//!
//! Desde Ajustes el usuario puede vaciar la carpeta del canal que elija.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Carpeta para las listas globales (BTTV, FFZ y 7TV globales).
pub const GLOBAL: &str = "_global";
/// Carpeta para los emotes propios del usuario en 7TV.
pub const MINE: &str = "_mine";
/// Pasado este tiempo se comprueba si los proveedores cambiaron la lista.
/// Mientras tanto se usa la caché sin tocar la red.
const RECHECK: Duration = Duration::from_secs(15 * 60);
/// Cambia cuando cambia el formato de las URL o de los archivos: las listas
/// viejas se ignoran y se vuelven a pedir una vez.
const FORMAT_VERSION: u32 = 2;

#[derive(Serialize, Deserialize)]
struct ListFile<T> {
    fetched_at: u64,
    #[serde(default)]
    version: u32,
    items: Vec<T>,
}

#[derive(Serialize)]
struct ListRef<'a, T> {
    fetched_at: u64,
    version: u32,
    items: &'a [T],
}

/// Resumen de la caché de un canal, para mostrarlo en Ajustes.
#[derive(Clone, Debug)]
pub struct Entry {
    pub channel: String,
    pub bytes: u64,
    pub files: usize,
    pub updated: Option<u64>,
}

/// Raíz de la caché (`<datos>/emotes`). Se crea si no existe.
pub fn root() -> PathBuf {
    let dir = crate::config::data_dir().join("emotes");
    let _ = fs::create_dir_all(&dir);
    dir
}

/// Carpeta de un canal concreto.
pub fn channel_dir(channel: &str) -> PathBuf {
    let mut name = crate::config::sanitize_name(channel);
    if name.is_empty() {
        name = "_canal".to_string();
    }
    root().join(name)
}

fn list_path(channel: &str, source: &str) -> PathBuf {
    channel_dir(channel)
        .join("listas")
        .join(format!("{source}.json"))
}

/// Ruta del archivo de una imagen de emote dentro del canal.
/// Solo se usa en las pruebas: el nombre debe coincidir con el de `images`.
#[cfg(test)]
pub fn image_file(channel: &str, url: &str) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hasher);
    let hash = hasher.finish();
    let clean = url.split(['?', '#']).next().unwrap_or(url);
    let ext = clean.rsplit('.').next().unwrap_or("");
    let ext = if !ext.is_empty()
        && ext.len() <= 5
        && ext.chars().all(|c| c.is_ascii_alphanumeric())
    {
        ext.to_ascii_lowercase()
    } else {
        "img".to_string()
    };
    channel_dir(channel)
        .join("img")
        .join(format!("{hash:016x}.{ext}"))
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or(0)
}

/// ¿Toca volver a preguntar por cambios?
pub fn is_stale(fetched_at: u64) -> bool {
    now_secs().saturating_sub(fetched_at) > RECHECK.as_secs()
}

/// Lee la lista guardada de un canal. Devuelve también cuándo se comprobó.
pub fn read_list<T: DeserializeOwned>(channel: &str, source: &str) -> Option<(u64, Vec<T>)> {
    let text = fs::read_to_string(list_path(channel, source)).ok()?;
    let file: ListFile<T> = serde_json::from_str(&text).ok()?;
    if file.version != FORMAT_VERSION {
        return None;
    }
    Some((file.fetched_at, file.items))
}

/// Guarda (o actualiza) la lista de un canal y su marca de tiempo.
pub fn write_list<T: Serialize>(channel: &str, source: &str, items: &[T]) {
    let path = list_path(channel, source);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let file = ListRef {
        fetched_at: now_secs(),
        version: FORMAT_VERSION,
        items,
    };
    if let Ok(json) = serde_json::to_string(&file) {
        let _ = fs::write(path, json);
    }
}

/// Escanea la caché y devuelve un resumen por canal (el que más ocupa primero).
pub fn scan() -> Vec<Entry> {
    let mut out = Vec::new();
    let Ok(dirs) = fs::read_dir(root()) else {
        return out;
    };
    for dir in dirs.flatten() {
        let path = dir.path();
        if !path.is_dir() {
            continue;
        }
        let channel = dir.file_name().to_string_lossy().to_string();
        let (bytes, files, updated) = measure(&path);
        out.push(Entry {
            channel,
            bytes,
            files,
            updated,
        });
    }
    out.sort_by_key(|entry| std::cmp::Reverse(entry.bytes));
    out
}

fn measure(dir: &Path) -> (u64, usize, Option<u64>) {
    let mut bytes = 0u64;
    let mut files = 0usize;
    let mut updated = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                bytes += meta.len();
                files += 1;
                if let Ok(modified) = meta.modified() {
                    if let Ok(since) = modified.duration_since(UNIX_EPOCH) {
                        updated = updated.max(since.as_secs());
                    }
                }
            }
        }
    }
    (bytes, files, if updated > 0 { Some(updated) } else { None })
}

/// Borra la caché de un canal y devuelve los bytes liberados.
pub fn clear_channel(channel: &str) -> u64 {
    let dir = channel_dir(channel);
    let bytes = measure(&dir).0;
    let _ = fs::remove_dir_all(dir);
    bytes
}

/// Borra toda la caché de emotes y devuelve los bytes liberados.
pub fn clear_all() -> u64 {
    let dir = root();
    let bytes = measure(&dir).0;
    let _ = fs::remove_dir_all(dir);
    bytes
}

/// Nombre legible para una carpeta de la caché.
pub fn label(channel: &str) -> String {
    match channel {
        GLOBAL => "Globales".to_string(),
        MINE => "Mis emotes".to_string(),
        other => format!("#{other}"),
    }
}

/// Tamaño legible (`12,3 MB`).
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_round_trip() {
        let channel = "__test_emote_cache";
        let items = vec!["uno".to_string(), "dos".to_string()];
        write_list(channel, "prueba", &items);
        let (at, read) = read_list::<String>(channel, "prueba").expect("lista guardada");
        assert_eq!(read, items);
        assert!(now_secs() - at < 60);
        assert!(!is_stale(at));
        let freed = clear_channel(channel);
        assert!(freed > 0);
        assert!(read_list::<String>(channel, "prueba").is_none());
    }

    #[test]
    fn image_paths_are_stable_and_scoped() {
        let a = image_file("canal", "https://cdn.7tv.app/emote/abc/2x.gif?x=1");
        let b = image_file("canal", "https://cdn.7tv.app/emote/abc/2x.gif?x=1");
        let c = image_file("otro", "https://cdn.7tv.app/emote/abc/2x.gif?x=1");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.extension().is_some_and(|ext| ext == "gif"));
    }
}
