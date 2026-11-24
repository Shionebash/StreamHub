use crate::theme::Theme;
use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Player {
    Mpv,
    Vlc,
}

#[allow(clippy::derivable_impls)]
impl Default for Player {
    fn default() -> Self {
        Self::Mpv
    }
}

impl Player {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mpv => "mpv",
            Self::Vlc => "VLC",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewMode {
    #[default]
    List,
    Grid,
}

impl ViewMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::List => "Lista",
            Self::Grid => "Cuadrícula",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RenderBackend {
    #[default]
    Glow,
    Wgpu,
}

impl RenderBackend {
    pub const ALL: [Self; 2] = [Self::Glow, Self::Wgpu];

    pub fn label(self) -> &'static str {
        match self {
            Self::Glow => "glow (OpenGL)",
            Self::Wgpu => "wgpu (DirectX 12 / Vulkan)",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub player: Player,
    /// Backend gráfico; se aplica al reiniciar la app.
    pub renderer: RenderBackend,
    /// Reproducir el stream dentro de la app (requiere backend glow).
    pub embedded: bool,
    pub quality: String,
    pub mpv: String,
    pub vlc: String,
    pub streamlink: String,
    pub ffmpeg: String,
    pub width: u32,
    pub height: u32,
    pub hwaccel: bool,
    pub hw_gpu: String,
    pub client_id: String,
    pub favorites: Vec<String>,
    /// Ruta del ejecutable de Chatterino (opcional).
    pub chatterino: String,
    pub accent: String,
    pub text_only: bool,
    pub theme: Theme,
    pub view: ViewMode,
    // --- Chat -------------------------------------------------------------
    /// Ancho del panel de chat en píxeles.
    pub chat_width: f32,
    /// Tamaño de letra de los mensajes.
    pub chat_font: f32,
    /// Mostrar insignias (badges) de los usuarios.
    pub chat_badges: bool,
    /// Mostrar la hora de cada mensaje.
    pub chat_time: bool,
    /// Alternar el fondo de las filas para leer mejor.
    pub chat_alt: bool,
    /// Sidebar de navegación contraído a solo iconos.
    pub sidebar_collapsed: bool,
    /// Ancho de la barra lateral expandida en píxeles.
    pub sidebar_width: f32,
    /// Mostrar tarjetas en modo compacto (más información en menos espacio).
    pub compact: bool,
    /// Personalización independiente de colores (fondo, tarjetas, acento, etc.).
    pub custom_colors: crate::theme::CustomColors,
    /// Descargar e instalar automáticamente nuevas versiones de GitHub Releases.
    #[serde(default = "default_true")]
    pub auto_update: bool,
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            player: Player::Mpv,
            renderer: RenderBackend::Glow,
            embedded: true,
            quality: "best".to_string(),
            mpv: "mpv".to_string(),
            vlc: "vlc".to_string(),
            streamlink: "streamlink".to_string(),
            ffmpeg: "ffmpeg".to_string(),
            width: 1920,
            height: 1080,
            hwaccel: false,
            hw_gpu: "nvidia".to_string(),
            client_id: String::new(),
            favorites: Vec::new(),
            chatterino: String::new(),
            accent: "#ff8fab".to_string(),
            text_only: false,
            theme: Theme::CozyDark,
            view: ViewMode::List,
            chat_width: 400.0,
            chat_font: 13.0,
            chat_badges: true,
            chat_time: true,
            chat_alt: true,
            sidebar_collapsed: false,
            sidebar_width: 240.0,
            compact: false,
            custom_colors: crate::theme::CustomColors::default(),
            auto_update: true,
        }
    }
}

pub fn project_dirs() -> ProjectDirs {
    ProjectDirs::from("com", "StreamHub", "StreamHub")
        .expect("no se pudo resolver el directorio de datos del usuario")
}

pub fn data_dir() -> PathBuf {
    let dir = project_dirs().data_dir().to_path_buf();
    let _ = fs::create_dir_all(&dir);
    dir
}

pub fn config_path() -> PathBuf {
    data_dir().join("config.json")
}

pub fn logs_dir() -> PathBuf {
    let dir = data_dir().join("logs");
    let _ = fs::create_dir_all(&dir);
    dir
}

pub fn log_path(channel: &str) -> PathBuf {
    logs_dir().join(format!("{}.log", sanitize_name(channel)))
}

pub fn sanitize_name(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect::<String>()
        .to_lowercase()
}

impl Config {
    pub fn load() -> Self {
        let path = config_path();
        if path.exists() {
            if let Ok(text) = fs::read_to_string(&path) {
                if let Ok(cfg) = serde_json::from_str::<Config>(text.trim_start_matches('\u{feff}')) {
                    return cfg;
                }
            }
        }
        // Migración: si existe config.json en el directorio de trabajo, importar lo básico.
        if let Some(legacy) = read_legacy_config() {
            // El token en texto plano del proyecto antiguo pasa al llavero de Windows.
            if let Some(token) = legacy.get("token").and_then(|value| value.as_str()) {
                if !token.trim().is_empty() {
                    let _ = save_token(token.trim());
                }
            }
            let cfg = Config::from_legacy(&legacy);
            let _ = cfg.save();
            return cfg;
        }
        let cfg = Config::default();
        let _ = cfg.save();
        cfg
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path();
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self)?;
        fs::write(&tmp, text).with_context(|| format!("no se pudo escribir {}", tmp.display()))?;
        fs::rename(&tmp, &path)
            .with_context(|| format!("no se pudo mover {}", tmp.display()))?;
        Ok(())
    }

    fn from_legacy(value: &serde_json::Value) -> Self {
        let mut cfg = Config::default();
        if let Some(v) = value.get("player").and_then(|v| v.as_str()) {
            cfg.player = if v.eq_ignore_ascii_case("vlc") { Player::Vlc } else { Player::Mpv };
        }
        if let Some(v) = value.get("quality").and_then(|v| v.as_str()) {
            cfg.quality = v.to_string();
        }
        if let Some(v) = value.get("mpv").and_then(|v| v.as_str()) {
            cfg.mpv = v.to_string();
        }
        if let Some(v) = value.get("vlc").and_then(|v| v.as_str()) {
            cfg.vlc = v.to_string();
        }
        if let Some(v) = value.get("sl").and_then(|v| v.as_str()) {
            cfg.streamlink = v.to_string();
        }
        if let Some(v) = value.get("ff").and_then(|v| v.as_str()) {
            cfg.ffmpeg = v.to_string();
        }
        if let Some(v) = value.get("w").and_then(|v| v.as_u64()) {
            cfg.width = v as u32;
        }
        if let Some(v) = value.get("h").and_then(|v| v.as_u64()) {
            cfg.height = v as u32;
        }
        if let Some(v) = value.get("hwaccel").and_then(|v| v.as_bool()) {
            cfg.hwaccel = v;
        }
        if let Some(v) = value.get("hwgpu").and_then(|v| v.as_str()) {
            cfg.hw_gpu = v.to_string();
        }
        if let Some(v) = value.get("clientId").and_then(|v| v.as_str()) {
            cfg.client_id = v.to_string();
        }
        if let Some(v) = value.get("accentColor").and_then(|v| v.as_str()) {
            cfg.accent = v.to_string();
        }
        if let Some(v) = value.get("textOnly").and_then(|v| v.as_bool()) {
            cfg.text_only = v;
        }
        if let Some(v) = value.get("theme").and_then(|v| v.as_str()) {
            cfg.theme = match v.to_ascii_lowercase().as_str() {
                "night" | "dark" | "oscuro" => Theme::Night,
                "forest" => Theme::Forest,
                "ocean" => Theme::Ocean,
                "sakura" => Theme::Sakura,
                _ => Theme::Postknight,
            };
        }
        if let Some(v) = value.get("view").and_then(|v| v.as_str()) {
            cfg.view = if v.eq_ignore_ascii_case("grid") {
                ViewMode::Grid
            } else {
                ViewMode::List
            };
        }
        cfg
    }
}

fn read_legacy_config() -> Option<serde_json::Value> {
    let path = std::env::current_dir().ok()?.join("config.json");
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

// El token vive en el Administrador de credenciales de Windows con el mismo
// formato que usaba la app Python (`StreamHub` / `oauth_token`).
pub use crate::credentials::{delete_token, load_token, save_token};

pub fn parse_hex_color(hex: &str) -> Option<egui::Color32> {
    let hex = hex.trim().trim_start_matches('#');
    if hex.len() == 6 {
        if let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&hex[0..2], 16),
            u8::from_str_radix(&hex[2..4], 16),
            u8::from_str_radix(&hex[4..6], 16),
        ) {
            return Some(egui::Color32::from_rgb(r, g, b));
        }
    }
    None
}

pub fn color_to_hex(color: egui::Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r(), color.g(), color.b())
}

pub fn accent_color(hex: &str) -> egui::Color32 {
    parse_hex_color(hex).unwrap_or(egui::Color32::from_rgb(255, 143, 171))
}
