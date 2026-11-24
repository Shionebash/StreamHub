//! Interfaz y estado de la aplicación nativa (egui).
//!
//! Todo se ejecuta en un solo hilo de UI. Las llamadas a Twitch, Streamlink y
//! el lanzamiento de reproductores se hacen en hilos cortos que devuelven el
//! resultado por un canal `mpsc`; así la interfaz nunca se bloquea.

use crate::anim;
use crate::chat;
use crate::config::{self, Config, Player, ViewMode};
use crate::emote_cache;
use crate::embedded::{self, EmbeddedPlayer};
use crate::emotes;
use crate::gql;
use crate::icons::{self, Icon};
use crate::images::ImageCache;
use crate::mining;
use crate::player::{self, Geometry, RunningStream};
use crate::sevent;
use crate::seven_tv;
use crate::theme::{self, Palette, Theme};
use crate::twitch::{FollowedChannel, Game, SearchResult, StreamInfo, TwitchClient, UserInfo};
use crate::updater;
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod chat_ui;

// ---------------------------------------------------------------------------
// Modelo
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Player,
    Queue,
    Favorites,
    Followed,
    Explore,
    Settings,
}

const TABS: [(Tab, &str); 6] = [
    (Tab::Player, "Reproductor"),
    (Tab::Queue, "Cola"),
    (Tab::Favorites, "Favoritos"),
    (Tab::Followed, "Seguidos"),
    (Tab::Explore, "Explorar"),
    (Tab::Settings, "Ajustes"),
];

impl Tab {
    fn icon(self) -> Icon {
        match self {
            Self::Player => Icon::Tv,
            Self::Queue => Icon::Queue,
            Self::Favorites => Icon::Heart,
            Self::Followed => Icon::Users,
            Self::Explore => Icon::Gamepad,
            Self::Settings => Icon::Settings,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Channel {
    name: String,
    #[serde(default)]
    display: String,
    #[serde(default)]
    audio_only: bool,
}

impl Channel {
    fn label(&self) -> &str {
        if self.display.is_empty() {
            &self.name
        } else {
            &self.display
        }
    }
}

#[derive(Clone, Debug, Default)]
struct LiveInfo {
    live: bool,
    viewers: u64,
    game: String,
    title: String,
    thumbnail_url: String,
    /// ID numérico del canal (lo usa 7TV para los emotes del canal).
    user_id: String,
}

#[derive(Serialize, Deserialize, Default)]
struct PersistedState {
    #[serde(default)]
    queue: Vec<Channel>,
}

fn state_path() -> PathBuf {
    config::data_dir().join("queue.json")
}

fn load_state() -> PersistedState {
    fs::read_to_string(state_path())
        .ok()
        .and_then(|text| serde_json::from_str(text.trim_start_matches('\u{feff}')).ok())
        .unwrap_or_default()
}

struct FollowedData {
    user: Option<UserInfo>,
    channels: Vec<FollowedChannel>,
}

enum Event {
    Live(Result<HashMap<String, StreamInfo>, String>),
    Followed(Result<FollowedData, String>),
    Games {
        page: usize,
        result: Result<crate::twitch::Page<Game>, String>,
    },
    Search(Result<Vec<SearchResult>, String>),
    Explore {
        game_id: String,
        page: usize,
        result: Result<crate::twitch::Page<StreamInfo>, String>,
    },
    Started {
        channel: String,
        result: Result<RunningStream, String>,
    },
    /// Identidad del usuario (login) para el chat.
    Identity(Result<UserInfo, String>),
    /// Emotes de un canal (canal, origen, emotes).
    ExternalEmotes(String, String, Vec<(String, String)>),
    /// Emotes globales de un proveedor externo (origen, emotes).
    ExternalGlobal(String, Vec<(String, String)>),
    /// Evento en vivo de 7TV.
    Seven(sevent::Event),
    /// ID numérico de un canal (consultado a Helix).
    ChannelId(String, String),
    /// Emotes disponibles para el usuario.
    Emotes(Vec<(String, String, String)>),
    /// Imágenes de perfil de Twitch (login, url).
    UserAvatars(Vec<(String, String)>),
    /// Emotes globales de 7TV.
    SevenGlobal(Vec<seven_tv::SevenEmote>),
    /// Emotes de 7TV del usuario.
    SevenMine(Vec<seven_tv::SevenEmote>),
    /// Emotes de 7TV de un canal.
    SevenChannel(String, Vec<seven_tv::SevenEmote>),
    /// Historial reciente de un canal (API de recent-messages).
    History(String, Result<Vec<chat::ChatEvent>, String>),
    /// Resultado de la comprobación de nueva versión en GitHub Releases.
    UpdateChecked {
        result: Result<updater::UpdateCheck, String>,
        silent: bool,
    },
    /// Resultado de la descarga de una actualización.
    UpdateDownloaded(Result<updater::StagedUpdate, String>),
}

enum UpdateState {
    Idle,
    Checking,
    UpToDate(Option<String>),
    Available(updater::ReleaseInfo),
    Downloading(String),
    Error(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowKind {
    Queue,
    Favorite,
    Followed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FollowFilter {
    All,
    Live,
    Offline,
}

enum RowAction {
    Play(String),
    PlayEmbedded(String),
    PlayExternal(String),
    Stop(String),
    ToggleAudio(String),
    ToggleFavorite(String),
    Logs(String),
    Remove(String),
    AddToQueue(String, String),
    OpenWeb(String),
    OpenChat(String),
}

// ---------------------------------------------------------------------------
// Aplicación
// ---------------------------------------------------------------------------

pub struct StreamHubApp {
    cfg: Config,
    /// Backend con el que arrancó esta instancia (el de `cfg` puede cambiar
    /// en Ajustes y aplicarse solo al reiniciar).
    renderer_active: config::RenderBackend,
    token: String,
    token_edit: String,
    token_visible: bool,

    tab: Tab,
    queue: Vec<Channel>,
    live: HashMap<String, LiveInfo>,

    followed: Vec<FollowedChannel>,
    followed_user: Option<UserInfo>,
    followed_filter: FollowFilter,

    games_pages: Vec<Vec<Game>>,
    games_page: usize,
    games_cursor: Option<String>,
    explore_game: Option<Game>,
    explore_pages: Vec<Vec<StreamInfo>>,
    explore_page: usize,
    explore_cursor: Option<String>,

    search_query: String,
    search_results: Vec<SearchResult>,

    new_channel: String,
    running: HashMap<String, RunningStream>,
    /// Reproductor integrado (solo uno a la vez).
    embedded: Option<EmbeddedPlayer>,
    embedded_channel: Option<String>,
    embedded_started: Option<Instant>,
    /// Contexto OpenGL de la ventana; se usa para liberar texturas del vídeo.
    gl: Option<Arc<eframe::glow::Context>>,
    embedded_warned: bool,
    // -- animaciones y gestos de la interfaz --------------------------------
    /// Pestaña mostrada en el frame anterior (para la transición).
    last_tab: Tab,
    /// Instante del último cambio de pestaña.
    tab_at: Instant,
    /// Instante de la última recarga de listas (entrada escalonada de tarjetas).
    list_gen_at: Instant,
    /// Última vez que el ratón se movió sobre el vídeo integrado.
    controls_at: Instant,
    /// Punto de inicio del arrastre del ancho del chat: (posición, ancho).
    chat_drag: Option<(egui::Pos2, f32)>,
    sidebar_drag: Option<(egui::Pos2, f32)>,
    launching: HashSet<String>,
    pending_stop: HashSet<String>,

    logs_open: bool,
    logs_channel: Option<String>,
    logs_text: String,
    logs_updated: Instant,

    toast: Option<(String, Instant)>,
    logo: Option<egui::TextureHandle>,
    images: ImageCache,
    global_search: String,

    tx: Sender<Event>,
    rx: Receiver<Event>,

    busy_live: bool,
    busy_followed: bool,
    followed_loaded: bool,
    busy_games: bool,
    games_loaded: bool,
    busy_search: bool,
    busy_explore: bool,

    update_state: UpdateState,
    pending_auto_update: Option<updater::ReleaseInfo>,

    pending_live_refresh: bool,
    last_live_refresh: Instant,

    // Chat -----------------------------------------------------------------
    chat_tx: tokio::sync::mpsc::UnboundedSender<chat::Command>,
    chat_rx: Receiver<chat::ChatEvent>,
    chat_events_tx: Sender<chat::ChatEvent>,
    chat_thread: Option<std::thread::JoinHandle<()>>,
    chat_messages: Vec<chat::ChatEvent>,
    chat_channels: Vec<String>,
    chat_channel: Option<String>,
    /// Carpeta de caché del canal abierto. Se calcula una vez por canal: antes
    /// se resolvía (`ProjectDirs` + `create_dir_all`) en cada frame.
    chat_cache_dir: Option<(String, std::path::PathBuf)>,
    /// Caché de segmentos ya parseados por mensaje (evita re-tokenizar cada frame).
    chat_seg_cache: HashMap<u64, Arc<chat_ui::CachedChatRow>>,
    chat_seg_key: (String, u64, bool, bool, theme::Theme, String),
    /// Alturas medidas por mensaje para virtualizar filas fuera del viewport.
    chat_row_heights: HashMap<u64, f32>,
    chat_layout_key: (String, u32, u32, bool, bool, u64),
    chat_input: String,
    chat_connected: bool,
    chat_stats: HashMap<String, ChatStats>,
    chat_emotes: HashMap<String, String>,
    chat_panel: bool,
    chat_tab: ChatTab,
    /// Login del usuario (para la sesión del chat).
    my_login: String,
    /// Emotes de Twitch del usuario (`id`, `nombre`, `canal dueño`). El canal
    /// dueño vacío indica un conjunto global; el resto son suscripciones.
    my_emotes: Vec<(String, String, String)>,
    /// Emotes de 7TV: globales, propios y por canal.
    seven_global: Vec<seven_tv::SevenEmote>,
    seven_mine: Vec<seven_tv::SevenEmote>,
    seven_channel: HashMap<String, Vec<seven_tv::SevenEmote>>,
    /// Emotes de BetterTTV y FrankerFaceZ (globales y por canal).
    external_global: Vec<(String, String)>,
    external_channel: HashMap<String, Vec<(String, String)>>,
    /// Listas externas por (canal, origen) para poder actualizar solo una.
    external_sources: HashMap<(String, String), Vec<(String, String)>>,
    /// Caché de emotes en disco: resumen para Ajustes.
    emote_cache_entries: Vec<emote_cache::Entry>,
    emote_cache_selected: String,
    emote_cache_scanned: bool,
    /// Emotes de un canal ya calculados para no recalcular la lista en cada
    /// frame. Se comparte con `Arc`: devolver la lista no copia miles de
    /// cadenas.
    emote_lists: HashMap<String, Arc<Vec<EmoteEntry>>>,
    emote_lists_dirty: bool,
    /// Cambia cuando se invalida `emote_lists`; el selector lo usa para
    /// reconstruir su filtro solo cuando hace falta.
    emote_generation: u64,
    /// ID numérico de los canales (para 7TV, BTTV y FFZ).
    channel_ids: HashMap<String, String>,
    /// Imagen de perfil por login (consultada a Helix).
    user_avatars: HashMap<String, String>,
    /// Logins de avatar ya solicitados (no repetir la consulta).
    avatars_pending: HashSet<String>,
    /// Eventos en vivo de 7TV.
    sevent_rx: Receiver<sevent::Event>,
    sevent_events_tx: Sender<sevent::Event>,
    sevent_channels: Vec<String>,
    sevent_stops: HashMap<String, Arc<std::sync::atomic::AtomicBool>>,
    /// Índice `nombre en minúsculas → URL` por canal, construido junto con
    /// `emote_lists` y usado para pintar emotes dentro de los mensajes.
    emote_indexes: HashMap<String, Arc<HashMap<String, String>>>,
    /// ID numérico del usuario (lo necesita 7TV).
    my_id: String,
    /// Filtro del selector de emotes.
    emote_search: String,
    /// Filtro por origen del emote (7TV o Twitch).
    emote_source: EmoteSource,
    /// Lista ya filtrada del selector; se reconstruye al cambiar canal,
    /// búsqueda, origen o la lista de emotes, no en cada frame.
    emote_picker_items: Vec<EmoteEntry>,
    emote_picker_for: String,
    emote_picker_source: EmoteSource,
    emote_picker_query: String,
    emote_picker_generation: u64,
    /// Conjuntos de emotes de la sesión.
    chat_emote_sets: Vec<String>,
    emote_picker: bool,
    /// Mantiene el chat pegado al final (se desactiva al subir).
    chat_follow: bool,
    /// Mensajes sin leer por canal.
    chat_unread: HashMap<String, u32>,
    /// Buscador/filtro de mensajes del canal actual.
    chat_filter: String,
    chat_filter_open: bool,
    /// Ajustes rápidos del chat.
    chat_settings_open: bool,
    /// Historial de mensajes enviados (flechas ↑/↓).
    chat_history: Vec<String>,
    chat_history_pos: Option<usize>,
    /// Estado de la autocompletado con Tab.
    chat_completion: Option<Completion>,
    /// Canales cuyo historial reciente ya se pidió.
    chat_history_requested: HashSet<String>,
    /// ID del mensaje al que se responde.
    chat_reply: Option<(String, String, String)>,
    /// Rectángulo del campo de escritura (ancla de los paneles flotantes).
    chat_input_rect: Option<egui::Rect>,
    /// Rectángulo del panel de chat (para que los flotantes no se salgan).
    chat_panel_rect: Option<egui::Rect>,
    /// Devolver el foco al campo de escritura tras insertar un emote.
    chat_focus_input: bool,

    // Puntos de canal -------------------------------------------------------
    mining_rx: Receiver<mining::Event>,
    mining_tx: Sender<mining::Command>,
    mining_events_tx: Sender<mining::Event>,
    mining_points: HashMap<String, u64>,
    mining_contexts: HashMap<String, gql::ChannelPointsContext>,
    mining_watch_ticks: HashMap<String, u64>,
    mining_chests: Vec<mining::Chest>,
    mining_drops: HashMap<String, (u64, u64)>,
    mining_log: Vec<String>,
    mining_connected: bool,
    mining_auth_error: Option<String>,
    mining_running: bool,
    mining_stop: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Popup de recompensas de puntos del canal abierto en el chat.
    points_popup_open: bool,
    /// ID de la recompensa seleccionada cuando requiere texto del usuario.
    points_selected_reward: Option<String>,
    /// Texto escrito por el usuario para canjear una recompensa con entrada.
    points_reward_input: String,
    /// Evita envíos duplicados mientras un canje está en curso.
    points_redeeming: bool,
}

/// Pestañas del panel lateral derecho.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChatTab {
    Chat,
    Events,
    Stats,
}

/// Contadores de eventos por canal.
#[derive(Clone, Copy, Debug, Default)]
struct ChatStats {
    messages: u32,
    subs: u32,
    gifts: u32,
    raids: u32,
    bits: u32,
}

/// Autocompletado del campo de escritura (Tab).
#[derive(Clone, Debug)]
struct Completion {
    /// Índice de carácter donde empieza la palabra que se completa.
    start: usize,
    candidates: Vec<String>,
    index: usize,
}

/// Origen de los emotes del selector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EmoteSource {
    All,
    SevenTv,
    Twitch,
    Bttv,
    Ffz,
}

/// Proveedor del que viene un emote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmoteOrigin {
    SevenTv,
    Twitch,
    Bttv,
    Ffz,
}

impl EmoteOrigin {
    pub fn label(self) -> &'static str {
        match self {
            Self::SevenTv => "7TV",
            Self::Twitch => "Twitch",
            Self::Bttv => "BTTV",
            Self::Ffz => "FFZ",
        }
    }
}

/// Un emote disponible para el chat: nombre, imagen, proveedor y canal
/// dueño (vacío = global o sin dueño concreto).
#[derive(Clone, Debug)]
pub struct EmoteEntry {
    pub name: String,
    pub url: String,
    pub origin: EmoteOrigin,
    /// Login del canal dueño (emotes de suscripción); vacío si es global.
    pub owner: String,
}

impl StreamHubApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let cfg = Config::load();
        let renderer_active = cfg.renderer;
        let token = config::load_token().unwrap_or_default();
        let state = load_state();
        let (tx, rx) = mpsc::channel();
        let logo = load_logo(&cc.egui_ctx);

        // Chat y puntos de canal: un hilo cada uno, con canales `mpsc`.
        let (chat_tx, chat_commands) = tokio::sync::mpsc::unbounded_channel::<chat::Command>();
        let (chat_events, chat_rx) = mpsc::channel::<chat::ChatEvent>();
        let (sevent_events, sevent_rx) = mpsc::channel::<sevent::Event>();
        let (mining_tx, mining_commands) = mpsc::channel::<mining::Command>();
        let (mining_events, mining_rx) = mpsc::channel::<mining::Event>();

        theme::install_fonts(&cc.egui_ctx);
        // Registrar fuentes de iconflow para los iconos de la app en egui 0.37
        for asset in iconflow::fonts() {
            let family = egui::FontFamily::Name(asset.family.into());
            cc.egui_ctx.add_font(egui::FontInsert::new(
                asset.family,
                egui::FontData::from_static(asset.bytes),
                vec![
                    egui::InsertFontFamily {
                        family: family.clone(),
                        priority: egui::epaint::text::FontPriority::Highest,
                    },
                    egui::InsertFontFamily {
                        family: egui::FontFamily::Proportional,
                        priority: egui::epaint::text::FontPriority::Lowest,
                    },
                    egui::InsertFontFamily {
                        family: egui::FontFamily::Monospace,
                        priority: egui::epaint::text::FontPriority::Lowest,
                    },
                ],
            ));
        }
        theme::apply_with_overrides(
            &cc.egui_ctx,
            cfg.theme,
            config::accent_color(&cfg.accent),
            &cfg.custom_colors,
        );

        let initial_theme = cfg.theme;
        let mut app = Self {
            cfg,
            renderer_active,
            token: token.clone(),
            token_edit: token.clone(),
            token_visible: false,
            tab: Tab::Queue,
            queue: state.queue,
            live: HashMap::new(),
            followed: Vec::new(),
            followed_user: None,
            followed_filter: FollowFilter::All,
            games_pages: Vec::new(),
            games_page: 0,
            games_cursor: None,
            explore_game: None,
            explore_pages: Vec::new(),
            explore_page: 0,
            explore_cursor: None,
            search_query: String::new(),
            search_results: Vec::new(),
            new_channel: String::new(),
            running: HashMap::new(),
            embedded: None,
            embedded_channel: None,
            embedded_started: None,
            gl: None,
            embedded_warned: false,
            last_tab: Tab::Player,
            tab_at: Instant::now(),
            list_gen_at: Instant::now(),
            controls_at: Instant::now(),
            chat_drag: None,
            sidebar_drag: None,
            launching: HashSet::new(),
            pending_stop: HashSet::new(),
            logs_open: false,
            logs_channel: None,
            logs_text: String::new(),
            logs_updated: Instant::now(),
            toast: None,
            logo,
            images: ImageCache::new(),
            global_search: String::new(),
            tx,
            rx,
            busy_live: false,
            busy_followed: false,
            followed_loaded: false,
            busy_games: false,
            games_loaded: false,
            busy_search: false,
            busy_explore: false,
            update_state: UpdateState::Idle,
            pending_auto_update: None,
            pending_live_refresh: true,
            last_live_refresh: Instant::now(),
            chat_tx,
            chat_rx,
            chat_events_tx: chat_events.clone(),
            chat_thread: None,
            chat_messages: Vec::new(),
            chat_channels: Vec::new(),
            chat_channel: None,
            chat_cache_dir: None,
            chat_seg_cache: HashMap::new(),
            chat_seg_key: (String::new(), u64::MAX, false, false, initial_theme, String::new()),
            chat_row_heights: HashMap::new(),
            chat_layout_key: (String::new(), 0, 0, false, false, u64::MAX),
            chat_input: String::new(),
            chat_connected: false,
            chat_stats: HashMap::new(),
            chat_emotes: HashMap::new(),
            chat_panel: false,
            chat_tab: ChatTab::Chat,
            my_login: String::new(),
            my_emotes: Vec::new(),
            seven_global: Vec::new(),
            seven_mine: Vec::new(),
            seven_channel: HashMap::new(),
            external_global: Vec::new(),
            external_channel: HashMap::new(),
            external_sources: HashMap::new(),
            emote_cache_entries: Vec::new(),
            emote_cache_selected: String::new(),
            emote_cache_scanned: false,
            channel_ids: HashMap::new(),
    user_avatars: HashMap::new(),
    avatars_pending: HashSet::new(),
            emote_lists: HashMap::new(),
            emote_lists_dirty: false,
            emote_generation: 0,
            sevent_rx,
            sevent_events_tx: sevent_events.clone(),
            sevent_channels: Vec::new(),
            sevent_stops: HashMap::new(),
            emote_indexes: HashMap::new(),
            my_id: String::new(),
            emote_search: String::new(),
            emote_source: EmoteSource::All,
            emote_picker_items: Vec::new(),
            emote_picker_for: String::new(),
            emote_picker_source: EmoteSource::All,
            emote_picker_query: String::new(),
            emote_picker_generation: u64::MAX,
            chat_emote_sets: Vec::new(),
            emote_picker: false,
            chat_follow: true,
            chat_unread: HashMap::new(),
            chat_filter: String::new(),
            chat_filter_open: false,
            chat_settings_open: false,
            chat_history: Vec::new(),
            chat_history_pos: None,
            chat_completion: None,
            chat_history_requested: HashSet::new(),
            chat_reply: None,
            chat_input_rect: None,
            chat_panel_rect: None,
            chat_focus_input: false,
            mining_rx,
            mining_tx,
            mining_events_tx: mining_events.clone(),
            mining_points: HashMap::new(),
            mining_contexts: HashMap::new(),
            mining_watch_ticks: HashMap::new(),
            mining_chests: Vec::new(),
            mining_drops: HashMap::new(),
            mining_log: Vec::new(),
            mining_connected: false,
            mining_auth_error: None,
            mining_running: false,
            mining_stop: None,
            points_popup_open: false,
            points_selected_reward: None,
            points_reward_input: String::new(),
            points_redeeming: false,
        };

        // El hilo de chat se arranca bajo demanda; los puntos de canal arrancan
        // automáticamente en cuanto hay credenciales.
        let _ = (&chat_commands, &mining_commands);

        if app.has_creds() {
            app.ensure_mining_started();
            app.spawn_followed(&cc.egui_ctx);
            if !app.queue.is_empty() || !app.cfg.favorites.is_empty() {
                app.spawn_live(&cc.egui_ctx);
            }
        }
        app.spawn_update_check(&cc.egui_ctx, true);

        // Atajo de desarrollo: abre un canal en el reproductor integrado
        // (`SH_AUTO_PLAY=canal`). Solo en compilaciones de depuración.
        #[cfg(debug_assertions)]
        {
            if let Ok(channel) = std::env::var("SH_AUTO_PLAY") {
                let channel = channel.trim().to_string();
                if !channel.is_empty() {
                    app.cfg.embedded = true;
                    app.start_channel(&cc.egui_ctx, &channel, false, None);
                }
            }
        }

        // Atajo de desarrollo para abrir un chat sin tocar la interfaz
        // (`SH_AUTO_CHAT=canal`). Solo en compilaciones de depuración.
        #[cfg(debug_assertions)]
        {
            if let Ok(channel) = std::env::var("SH_AUTO_CHAT") {
                if !channel.trim().is_empty() {
                    app.chat_panel = true;
                    app.chat_join(channel.trim());
                    if std::env::var("SH_AUTO_EMOTES").is_ok() {
                        app.emote_picker = true;
                    }
                }
            }
        }

        // Login del usuario: hace falta para que el chat use su sesión
        // (badges, emotes suscritos y envío de mensajes).
        if app.has_creds() {
            let (client_id, token) = app.credentials().unwrap();
            let events = app.tx.clone();
            std::thread::Builder::new()
                .name("identity".into())
                .spawn(move || {
                    let result = match TwitchClient::new(&client_id, &token) {
                        Ok(client) => match client.get_me() {
                            Ok(Some(user)) => Ok(user),
                            Ok(None) => {
                                Err("el token no pertenece a ningún usuario".to_string())
                            }
                            Err(err) => Err(format!("{err:#}")),
                        },
                        Err(err) => Err(format!("{err:#}")),
                    };
                    let _ = events.send(Event::Identity(result));
                })
                .ok();
        }

        app
    }

    // -- utilidades ---------------------------------------------------------

    fn has_creds(&self) -> bool {
        !self.cfg.client_id.trim().is_empty() && !self.token.is_empty()
    }

    fn credentials(&self) -> Option<(String, String)> {
        if self.has_creds() {
            Some((self.cfg.client_id.trim().to_string(), self.token.clone()))
        } else {
            None
        }
    }

    fn set_toast(&mut self, message: impl Into<String>) {
        self.toast = Some((message.into(), Instant::now()));
    }

    fn palette(&self) -> Palette {
        theme::palette_with_overrides(
            self.cfg.theme,
            config::accent_color(&self.cfg.accent),
            &self.cfg.custom_colors,
        )
    }

    fn apply_theme(&self, ctx: &egui::Context) {
        theme::apply_with_overrides(
            ctx,
            self.cfg.theme,
            config::accent_color(&self.cfg.accent),
            &self.cfg.custom_colors,
        );
    }

    #[allow(dead_code)]
    fn toggle_theme(&mut self, ctx: &egui::Context) {
        // Toggle between dark and light Cozy Pastel themes.
        self.cfg.theme = if self.cfg.theme.is_dark() {
            Theme::CozyLight
        } else {
            Theme::CozyDark
        };
        if self.cfg.custom_colors.accent.is_none() {
            self.cfg.accent = self.cfg.theme.default_accent().to_string();
        }
        self.apply_theme(ctx);
        let _ = self.cfg.save();
    }

    #[allow(dead_code)]
    /// Switch theme palette.
    fn set_theme(&mut self, ctx: &egui::Context, mode: Theme) {
        if self.cfg.theme == mode {
            return;
        }
        if self
            .cfg
            .accent
            .eq_ignore_ascii_case(self.cfg.theme.default_accent())
        {
            self.cfg.accent = mode.default_accent().to_string();
        }
        self.cfg.theme = mode;
        self.apply_theme(ctx);
        let _ = self.cfg.save();
    }

    fn tab_count(&self, tab: Tab) -> Option<usize> {
        match tab {
            Tab::Player => None,
            Tab::Queue => Some(self.queue.len()),
            Tab::Favorites => Some(self.cfg.favorites.len()),
            Tab::Followed => Some(self.followed.len()),
            Tab::Explore => Some(self.games_pages.iter().map(Vec::len).sum()),
            Tab::Settings => None,
        }
    }

    fn is_favorite(&self, name: &str) -> bool {
        self.cfg
            .favorites
            .iter()
            .any(|f| f.eq_ignore_ascii_case(name))
    }

    /// Número de reproductores activos, incluido el integrado.
    fn active_players(&self) -> usize {
        self.running.len() + usize::from(self.embedded.is_some())
    }

    /// Indica si el canal está reproduciéndose (externo o integrado).
    fn is_playing(&self, name: &str) -> bool {
        self.running.contains_key(name) || self.embedded_channel.as_deref() == Some(name)
    }

    fn display_for(&self, name: &str) -> String {
        self.queue
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name))
            .map(|c| c.label().to_string())
            .unwrap_or_else(|| name.to_string())
    }

    fn save_state(&self) {
        let state = PersistedState {
            queue: self.queue.clone(),
        };
        let Ok(text) = serde_json::to_string_pretty(&state) else {
            return;
        };
        let path = state_path();
        let tmp = path.with_extension("json.tmp");
        if fs::write(&tmp, text).is_ok() {
            let _ = fs::rename(&tmp, &path);
        }
    }

    fn save_config(&mut self, ctx: &egui::Context) {
        self.cfg.client_id = self.cfg.client_id.trim().to_string();
        match self.cfg.save() {
            Ok(()) => {
                self.apply_theme(ctx);
                self.ensure_mining_started();
                if self.has_creds() && self.followed.is_empty() && !self.busy_followed {
                    self.followed_loaded = false;
                    self.spawn_followed(ctx);
                }
                self.set_toast("Configuración guardada");
            }
            Err(err) => self.set_toast(format!("No se pudo guardar la configuración: {err:#}")),
        }
    }

    /// Guarda la configuración y lanza una instancia nueva de la app.
    fn restart_app(&mut self) {
        if let Err(err) = self.cfg.save() {
            self.set_toast(format!("No se pudo guardar la configuración: {err:#}"));
            return;
        }
        let Ok(exe) = std::env::current_exe() else {
            self.set_toast("No se pudo localizar el ejecutable de StreamHub");
            return;
        };
        match std::process::Command::new(exe).spawn() {
            Ok(_) => std::process::exit(0),
            Err(err) => self.set_toast(format!("No se pudo reiniciar: {err}")),
        }
    }

    fn refresh_logs(&mut self) {
        if let Some(channel) = self.logs_channel.clone() {
            let path = self
                .running
                .get(&channel)
                .map(|stream| stream.log_path.clone())
                .unwrap_or_else(|| config::log_path(&channel));
            self.logs_text = player::read_tail(&path, 250);
            self.logs_updated = Instant::now();
        }
    }

    // -- cola ---------------------------------------------------------------

    fn add_channel(&mut self, raw: &str, ctx: &egui::Context) {
        self.add_channel_named(raw, "", ctx);
    }

    fn add_channel_named(&mut self, raw: &str, display: &str, ctx: &egui::Context) {
        let name = raw.trim().trim_start_matches('#').to_lowercase();
        if name.is_empty() {
            return;
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            self.set_toast("Nombre de canal inválido");
            return;
        }
        if self.queue.iter().any(|c| c.name == name) {
            self.set_toast(format!("{name} ya está en la cola"));
            return;
        }
        let display = if display.trim().is_empty() {
            name.clone()
        } else {
            display.trim().to_string()
        };
        self.queue.push(Channel {
            name: name.clone(),
            display,
            audio_only: false,
        });
        self.save_state();
        self.set_toast(format!("{name} añadido a la cola"));
        if self.has_creds() {
            self.spawn_live(ctx);
        }
    }

    fn remove_channel(&mut self, name: &str) {
        if self.running.contains_key(name) || self.launching.contains(name) {
            self.stop_channel(name);
        }
        self.queue.retain(|c| !c.name.eq_ignore_ascii_case(name));
        self.save_state();
    }

    fn toggle_favorite(&mut self, name: &str) {
        let name = name.to_lowercase();
        if let Some(pos) = self
            .cfg
            .favorites
            .iter()
            .position(|f| f.eq_ignore_ascii_case(&name))
        {
            self.cfg.favorites.remove(pos);
            self.set_toast(format!("{name} quitado de favoritos"));
        } else {
            self.cfg.favorites.push(name.clone());
            self.set_toast(format!("{name} añadido a favoritos"));
        }
        self.cfg.favorites.sort();
        self.cfg.favorites.dedup();
        let _ = self.cfg.save();
    }

    fn toggle_audio(&mut self, ctx: &egui::Context, name: &str) {
        if self.launching.contains(name) {
            self.set_toast("Espera a que el reproductor inicie para cambiar el modo");
            return;
        }
        let Some(pos) = self
            .queue
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
        else {
            return;
        };
        self.queue[pos].audio_only = !self.queue[pos].audio_only;
        let audio_only = self.queue[pos].audio_only;
        self.save_state();

        if self.running.contains_key(name) || self.embedded_channel.as_deref() == Some(name) {
            self.stop_channel(name);
            self.start_channel(ctx, name, audio_only, None);
        }
    }

    // -- reproductores ------------------------------------------------------

    fn start_channel(
        &mut self,
        ctx: &egui::Context,
        name: &str,
        audio_only: bool,
        geometry: Option<Geometry>,
    ) {
        let name = name.trim().trim_start_matches('#').to_lowercase();
        if name.is_empty() || self.running.contains_key(&name) || self.launching.contains(&name) {
            return;
        }
        if self.embedded_channel.as_deref() == Some(name.as_str()) {
            self.tab = Tab::Player;
            return;
        }
        if self.cfg.quality.trim().is_empty() {
            self.cfg.quality = "best".to_string();
        }

        if self.cfg.embedded && geometry.is_none() && self.start_embedded(ctx, &name, audio_only) {
            return;
        }

        self.start_external(ctx, &name, audio_only, geometry);
    }

    /// Arranca la reproducción integrada. Devuelve `false` si no es posible
    /// (backend distinto de glow o fallo de libmpv) para usar el externo.
    fn start_embedded(&mut self, ctx: &egui::Context, name: &str, audio_only: bool) -> bool {
        let name = name.trim().trim_start_matches('#').to_lowercase();
        if name.is_empty() {
            return false;
        }
        if self.embedded_channel.as_deref() == Some(name.as_str()) {
            self.tab = Tab::Player;
            return true;
        }
        if self.renderer_active != config::RenderBackend::Glow {
            if !self.embedded_warned {
                self.embedded_warned = true;
                self.set_toast(
                    "La reproducción integrada requiere el backend glow; se abrirá el reproductor externo.",
                );
            }
            return false;
        }
        self.stop_embedded();
        match EmbeddedPlayer::new(ctx, &self.cfg, &self.token, &name, audio_only) {
            Ok(player) => {
                self.embedded = Some(player);
                self.embedded_channel = Some(name.clone());
                self.embedded_started = Some(Instant::now());
                self.tab = Tab::Player;
                self.set_toast(format!("Reproduciendo {name} dentro de la app"));
                self.fetch_avatars(&[name.clone()]);
                self.watch_chat(&name);
                self.mining_watch(&name);
                ctx.request_repaint();
                true
            }
            Err(err) => {
                self.set_toast(format!(
                    "No se pudo iniciar el reproductor integrado: {err}. Se usará el externo."
                ));
                false
            }
        }
    }

    /// Deja preparado el chat del canal en reproducción (sin abrir el panel).
    fn watch_chat(&mut self, name: &str) {
        let name = name.trim().trim_start_matches('#').to_lowercase();
        if name.is_empty() {
            return;
        }
        self.chat_join(&name);
        self.chat_channel = Some(name);
        self.chat_follow = true;
    }

    /// Lanza el reproductor externo (VLC/mpv), independientemente del modo.
    fn start_external(
        &mut self,
        ctx: &egui::Context,
        name: &str,
        audio_only: bool,
        geometry: Option<Geometry>,
    ) {
        if name.is_empty() || self.running.contains_key(name) || self.launching.contains(name) {
            return;
        }
        self.launching.insert(name.to_string());
        let cfg = self.cfg.clone();
        let token = self.token.clone();
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let thread_name = name.to_string();

        std::thread::spawn(move || {
            let result = player::launch_at(
                &cfg,
                &thread_name,
                &cfg.quality,
                audio_only,
                &token,
                geometry,
            )
            .map_err(|err| format!("{err:#}"));
            let _ = tx.send(Event::Started {
                channel: thread_name,
                result,
            });
            ctx.request_repaint();
        });

        self.set_toast(format!("Iniciando {name}..."));
    }

    /// Cierra el reproductor integrado y libera su textura OpenGL.
    fn stop_embedded(&mut self) {
        if let Some(player) = self.embedded.take() {
            player.close();
            if let Some(gl) = &self.gl {
                player.destroy(gl);
            }
        }
        let prev = self.embedded_channel.take();
        self.embedded_started = None;
        if let Some(channel) = prev {
            self.mining_unwatch(&channel);
        }
    }

    /// Abre en VLC/mpv el canal que se está viendo dentro de la app.
    fn open_embedded_external(&mut self, ctx: &egui::Context) {
        let Some(channel) = self.embedded_channel.clone() else {
            return;
        };
        let audio_only = self
            .embedded
            .as_ref()
            .map(|player| player.snapshot().audio_only)
            .unwrap_or(false);
        self.stop_embedded();
        self.start_external(ctx, &channel, audio_only, None);
    }

    fn stop_channel(&mut self, name: &str) {
        if self.embedded_channel.as_deref() == Some(name) {
            self.stop_embedded();
            self.set_toast(format!("{name} detenido"));
            return;
        }
        if let Some(mut stream) = self.running.remove(name) {
            player::stop(&mut stream);
            self.mining_unwatch(name);
            self.set_toast(format!("{name} detenido"));
        } else if self.launching.contains(name) {
            self.pending_stop.insert(name.to_string());
        }
    }

    fn stop_all(&mut self) {
        self.stop_embedded();
        let names: Vec<String> = self.running.keys().cloned().collect();
        for name in names {
            self.stop_channel(&name);
        }
        for name in self.launching.iter() {
            self.pending_stop.insert(name.clone());
        }
        if self.running.is_empty() && self.launching.is_empty() {
            self.set_toast("No hay reproductores activos");
        } else {
            self.set_toast("Reproductores detenidos");
        }
    }

    fn playable_channels(&self) -> Vec<(String, bool)> {
        let any_live = self.live.values().any(|info| info.live);
        self.queue
            .iter()
            .filter(|channel| {
                !any_live
                    || self
                        .live
                        .get(&channel.name.to_lowercase())
                        .map(|info| info.live)
                        .unwrap_or(false)
            })
            .map(|channel| (channel.name.clone(), channel.audio_only))
            .collect()
    }

    fn start_all(&mut self, ctx: &egui::Context) {
        let channels = self.playable_channels();
        if channels.is_empty() {
            self.set_toast("No hay canales para reproducir");
            return;
        }
        for (name, audio_only) in channels {
            self.start_external(ctx, &name, audio_only, None);
        }
    }

    fn start_grid(&mut self, ctx: &egui::Context) {
        let channels = self.playable_channels();
        if channels.is_empty() {
            self.set_toast("No hay canales para abrir en cuadrícula");
            return;
        }
        let total = channels.len();
        for (index, (name, audio_only)) in channels.into_iter().enumerate() {
            let geometry = grid_geometry(ctx, index, total);
            self.start_channel(ctx, &name, audio_only, Some(geometry));
        }
        self.set_toast(format!("Abriendo {total} ventana(s) en cuadrícula"));
    }

    fn shutdown(&mut self) {
        self.stop_embedded();
        self.stop_mining_internal();
        for (_, stop) in self.sevent_stops.drain() {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.sevent_channels.clear();
        for (_, mut stream) in self.running.drain() {
            player::stop(&mut stream);
        }
        self.launching.clear();
        self.pending_stop.clear();
    }

    // -- tareas en segundo plano -------------------------------------------

    fn spawn_live(&mut self, ctx: &egui::Context) {
        if self.busy_live {
            return;
        }
        let Some((client_id, token)) = self.credentials() else {
            self.set_toast("Configura Client ID y token en Ajustes");
            return;
        };

        let mut logins: Vec<String> = Vec::new();
        for channel in &self.queue {
            push_login(&mut logins, &channel.name);
        }
        for favorite in &self.cfg.favorites {
            push_login(&mut logins, favorite);
        }
        for followed in &self.followed {
            push_login(&mut logins, &followed.broadcaster_login);
        }

        if logins.is_empty() {
            self.set_toast("No hay canales para consultar");
            return;
        }

        self.busy_live = true;
        self.last_live_refresh = Instant::now();
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = TwitchClient::new(&client_id, &token)
                .and_then(|client| {
                    let mut map = client.get_streams(&logins)?;
                    for login in &logins {
                        map.entry(login.clone()).or_insert_with(|| StreamInfo {
                            user_login: login.clone(),
                            ..Default::default()
                        });
                    }
                    Ok(map)
                })
                .map_err(|err| format!("{err:#}"));
            let _ = tx.send(Event::Live(result));
            ctx.request_repaint();
        });
    }

    fn spawn_followed(&mut self, ctx: &egui::Context) {
        if self.busy_followed {
            return;
        }
        let Some((client_id, token)) = self.credentials() else {
            self.set_toast("Configura Client ID y token en Ajustes");
            return;
        };
        self.busy_followed = true;
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = fetch_followed(&client_id, &token);
            let _ = tx.send(Event::Followed(result));
            ctx.request_repaint();
        });
    }

    fn spawn_games(&mut self, ctx: &egui::Context, page: usize) {
        if page < self.games_pages.len() {
            self.games_page = page;
            self.list_gen_at = Instant::now();
            return;
        }
        if self.busy_games {
            return;
        }
        let Some((client_id, token)) = self.credentials() else {
            self.set_toast("Configura Client ID y token en Ajustes");
            return;
        };
        let after = if page == 0 {
            self.games_pages.clear();
            self.games_page = 0;
            self.games_cursor = None;
            None
        } else {
            let Some(cursor) = self.games_cursor.clone() else {
                return;
            };
            Some(cursor)
        };
        self.busy_games = true;
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = TwitchClient::new(&client_id, &token)
                .and_then(|client| client.get_top_games(after.as_deref()))
                .map_err(|err| format!("{err:#}"));
            let _ = tx.send(Event::Games { page, result });
            ctx.request_repaint();
        });
    }

    fn reload_games(&mut self, ctx: &egui::Context) {
        self.games_pages.clear();
        self.games_page = 0;
        self.games_cursor = None;
        self.spawn_games(ctx, 0);
    }

    fn spawn_search(&mut self, ctx: &egui::Context, query: &str) {
        if self.busy_search {
            return;
        }
        let Some((client_id, token)) = self.credentials() else {
            self.set_toast("Configura Client ID y token en Ajustes");
            return;
        };
        let query = query.trim().to_string();
        if query.is_empty() {
            return;
        }
        self.busy_search = true;
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = TwitchClient::new(&client_id, &token)
                .and_then(|client| client.search_channels(&query))
                .map_err(|err| format!("{err:#}"));
            let _ = tx.send(Event::Search(result));
            ctx.request_repaint();
        });
    }

    fn spawn_explore(&mut self, ctx: &egui::Context, page: usize) {
        let Some(game) = self.explore_game.clone() else {
            return;
        };
        if page < self.explore_pages.len() {
            self.explore_page = page;
            self.list_gen_at = Instant::now();
            return;
        }
        if self.busy_explore {
            return;
        }
        let Some((client_id, token)) = self.credentials() else {
            self.set_toast("Configura Client ID y token en Ajustes");
            return;
        };
        let after = if page == 0 {
            self.explore_pages.clear();
            self.explore_page = 0;
            self.explore_cursor = None;
            None
        } else {
            let Some(cursor) = self.explore_cursor.clone() else {
                return;
            };
            Some(cursor)
        };
        self.busy_explore = true;
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let game_id = game.id.clone();
        std::thread::spawn(move || {
            let result = TwitchClient::new(&client_id, &token)
                .and_then(|client| client.get_streams_by_game(&game_id, after.as_deref()))
                .map_err(|err| format!("{err:#}"));
            let _ = tx.send(Event::Explore {
                game_id,
                page,
                result,
            });
            ctx.request_repaint();
        });
    }

    fn reload_explore(&mut self, ctx: &egui::Context) {
        self.explore_pages.clear();
        self.explore_page = 0;
        self.explore_cursor = None;
        self.spawn_explore(ctx, 0);
    }

    fn refresh_current(&mut self, ctx: &egui::Context) {
        self.list_gen_at = Instant::now();
        match self.tab {
            Tab::Queue | Tab::Favorites => self.spawn_live(ctx),
            Tab::Followed => self.spawn_followed(ctx),
            Tab::Explore => {
                if self.explore_game.is_some() {
                    self.reload_explore(ctx);
                } else {
                    self.reload_games(ctx);
                }
            }
            Tab::Settings => {}
            Tab::Player => {}
        }
    }

    fn spawn_update_check(&mut self, ctx: &egui::Context, silent: bool) {
        if matches!(
            self.update_state,
            UpdateState::Checking | UpdateState::Downloading(_)
        ) {
            return;
        }
        self.update_state = UpdateState::Checking;
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = updater::check_latest().map_err(|err| format!("{err:#}"));
            let _ = tx.send(Event::UpdateChecked { result, silent });
            ctx.request_repaint();
        });
    }

    fn spawn_update_download(&mut self, ctx: &egui::Context, release: updater::ReleaseInfo) {
        if matches!(self.update_state, UpdateState::Downloading(_)) {
            return;
        }
        if release.download_url.is_none() {
            open_url(&release.html_url);
            self.set_toast(format!(
                "Abriendo GitHub Releases para {}",
                release.version_label
            ));
            return;
        }
        let version = release.version_label.clone();
        self.update_state = UpdateState::Downloading(version.clone());
        self.set_toast(format!("Descargando StreamHub {version}..."));
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = updater::download_and_stage(&release).map_err(|err| format!("{err:#}"));
            let _ = tx.send(Event::UpdateDownloaded(result));
            ctx.request_repaint();
        });
    }

    // -- eventos ------------------------------------------------------------

    fn handle_event(&mut self, event: Event) {
        // Los datos recién llegados reaniman la entrada escalonada de tarjetas.
        if matches!(
            event,
            Event::Live(_)
                | Event::Followed(_)
                | Event::Games { .. }
                | Event::Search(_)
                | Event::Explore { .. }
        ) {
            self.list_gen_at = Instant::now();
        }
        match event {
            Event::Live(result) => {
                self.busy_live = false;
                match result {
                    Ok(streams) => {
                        let mut map = HashMap::with_capacity(streams.len());
                        for (login, stream) in streams {
                            let is_live = stream.kind.eq_ignore_ascii_case("live");
                            let was_live = self
                                .live
                                .get(&login.to_lowercase())
                                .map(|info| info.live)
                                .unwrap_or(false);
                            // Notificación nativa cuando un canal se pone en vivo
                            if is_live && !was_live {
                                let display = if !stream.user_name.is_empty() {
                                    stream.user_name.clone()
                                } else {
                                    login.clone()
                                };
                                let game = if !stream.game_name.is_empty() {
                                    format!(" - {}", stream.game_name)
                                } else {
                                    String::new()
                                };
                                show_windows_notification(
                                    &format!("{display} está en vivo{game}"),
                                    &stream.title,
                                );
                            }
                            map.insert(
                                login.to_lowercase(),
                                LiveInfo {
                                    live: is_live,
                                    viewers: stream.viewer_count,
                                    game: stream.game_name,
                                    title: stream.title,
                                    thumbnail_url: stream.thumbnail_url,
                                    user_id: stream.user_id,
                                },
                            );
                        }
                        self.live = map;
                    }
                    Err(err) => self.set_toast(format!("No se pudo actualizar el estado: {err}")),
                }
            }
            Event::Followed(result) => {
                self.busy_followed = false;
                self.followed_loaded = true;
                match result {
                    Ok(data) => {
                        self.followed = data.channels;
                        self.followed_user = data.user;
                        self.pending_live_refresh = true;
                        // El chat necesita el login para usar la sesión del usuario.
                        self.chat_identity();
                        self.set_toast(format!("{} canales seguidos", self.followed.len()));
                    }
                    Err(err) => self.set_toast(format!("No se pudieron cargar los seguidos: {err}")),
                }
            }
            Event::Identity(result) => {                match result {
                    Ok(user) => {
                        self.my_login = user.login.to_lowercase();
                        self.my_id = user.id.clone();
                        self.chat_identity();
                        self.load_seven_mine();
                    }
                    Err(err) => {
                        self.set_toast(format!("No se pudo iniciar la sesión de chat: {err}"));
                    }
                }
            }
            Event::Emotes(emotes) => {
                // Pide los avatares de los canales dueños de los emotes para
                // mostrarlos en el selector.
                let owners: Vec<String> = emotes
                    .iter()
                    .map(|(_, _, owner)| owner.clone())
                    .filter(|owner| !owner.is_empty())
                    .collect();
                self.my_emotes = emotes;
                self.emote_lists_dirty = true;
                self.fetch_avatars(&owners);
            }
            Event::History(channel, result) => {
                if let Ok(events) = result {
                    self.merge_history(&channel, events);
                }
            }
            Event::ExternalGlobal(source, emotes) => {
                // Igual que en los canales, cada origen se guarda aparte y se
                // combinan: al actualizar BTTV no se pierden los de FFZ.
                self.external_sources.insert(
                    (emote_cache::GLOBAL.to_string(), source),
                    emotes,
                );
                let mut seen: HashSet<String> = HashSet::new();
                self.external_global = self
                    .external_sources
                    .iter()
                    .filter(|((name, _), _)| name == emote_cache::GLOBAL)
                    .flat_map(|(_, list)| list.clone())
                    .filter(|(name, _)| seen.insert(name.clone()))
                    .collect();
                self.emote_lists_dirty = true;
            }
            Event::ExternalEmotes(channel, source, emotes) => {
                // Cada origen se guarda por separado y luego se combinan, para
                // que al actualizar BTTV no se pierdan los de FFZ.
                self.external_sources
                    .insert((channel.clone(), source), emotes);
                let merged: Vec<(String, String)> = self
                    .external_sources
                    .iter()
                    .filter(|((name, _), _)| name == &channel)
                    .flat_map(|(_, list)| list.clone())
                    .collect();
                let mut seen: HashSet<String> = HashSet::new();
                self.external_channel.insert(
                    channel,
                    merged
                        .into_iter()
                        .filter(|(name, _)| seen.insert(name.clone()))
                        .collect(),
                );
                self.emote_lists_dirty = true;
            }
            Event::ChannelId(channel, id) => {
                let channel = channel.to_lowercase();
                self.channel_ids.insert(channel.clone(), id);
                // Ya con el ID se pueden pedir los emotes y los eventos en vivo.
                self.load_external_emotes(&channel);
                self.load_seven_channel(&channel);
                self.start_sevent(&channel);
            }
            Event::UserAvatars(list) => {
                for (login, url) in list {
                    if !url.is_empty() {
                        self.user_avatars.insert(login, url);
                    }
                }
            }
            Event::Seven(event) => match event {
                sevent::Event::Connected(connected) => {
                    if connected {
                        self.set_toast("7TV en vivo conectado");
                    }
                }
                sevent::Event::EmotesUpdated { channel, emotes } => {
                    let mut list = self
                        .seven_channel
                        .get(&channel)
                        .cloned()
                        .unwrap_or_default();
                    for emote in emotes {
                        if !list.iter().any(|existing| existing.id == emote.id) {
                            list.push(emote);
                        }
                    }
                    // Los emotes nuevos también se guardan en la caché del
                    // canal para la próxima vez.
                    emote_cache::write_list(&channel, "7tv", &list);
                    self.seven_channel.insert(channel.clone(), list);
                    self.emote_lists_dirty = true;
                    self.set_toast(format!("7TV: nuevos emotes en #{channel}"));
                }
                sevent::Event::Cosmetic { channel, name } => {
                    self.set_toast(format!("7TV: {name} en #{channel}"));
                }
            },
            Event::SevenGlobal(emotes) => {
                self.seven_global = emotes;
                self.emote_lists_dirty = true;
            }
            Event::SevenMine(emotes) => {
                self.seven_mine = emotes;
                self.emote_lists_dirty = true;
            }
            Event::SevenChannel(channel, emotes) => {
                self.seven_channel.insert(channel, emotes);
                self.emote_lists_dirty = true;
            }
            Event::Games { page, result } => {
                self.busy_games = false;
                self.games_loaded = true;
                match result {
                    Ok(data) => {
                        if page == 0 {
                            self.games_pages.clear();
                        }
                        if data.items.is_empty() && page > 0 {
                            self.games_cursor = None;
                            self.set_toast("No hay más categorías disponibles");
                        } else {
                            let count = data.items.len();
                            self.games_pages.push(data.items);
                            self.games_page = self.games_pages.len().saturating_sub(1);
                            self.games_cursor = data.cursor;
                            self.set_toast(format!(
                                "Página {} · {} categorías",
                                self.games_page + 1,
                                count
                            ));
                        }
                    }
                    Err(err) => self.set_toast(format!("No se pudieron cargar los juegos: {err}")),
                }
            }
            Event::Search(result) => {
                self.busy_search = false;
                match result {
                    Ok(results) => {
                        self.search_results = results;
                        self.set_toast(format!("{} resultado(s)", self.search_results.len()));
                    }
                    Err(err) => self.set_toast(format!("La búsqueda falló: {err}")),
                }
            }
            Event::Explore {
                game_id,
                page,
                result,
            } => {
                if self
                    .explore_game
                    .as_ref()
                    .map(|game| game.id == game_id)
                    .unwrap_or(false)
                {
                    self.busy_explore = false;
                    match result {
                        Ok(data) => {
                            if page == 0 {
                                self.explore_pages.clear();
                            }
                            if data.items.is_empty() && page > 0 {
                                self.explore_cursor = None;
                                self.set_toast("No hay más streams en esta categoría");
                            } else {
                                self.explore_pages.push(data.items);
                                self.explore_page = self.explore_pages.len().saturating_sub(1);
                                self.explore_cursor = data.cursor;
                            }
                        }
                        Err(err) => {
                            if page == 0 {
                                self.explore_pages.clear();
                                self.explore_page = 0;
                                self.explore_cursor = None;
                            }
                            self.set_toast(format!("No se pudo explorar la categoría: {err}"));
                        }
                    }
                }
            }
            Event::Started { channel, result } => {
                self.launching.remove(&channel);
                match result {
                    Ok(mut stream) => {
                        if self.pending_stop.remove(&channel) {
                            player::stop(&mut stream);
                            self.set_toast(format!("{channel} detenido"));
                        } else {
                            // El chat queda listo en la pestaña del canal.
                            let name = channel.clone();
                            self.running.insert(channel, stream);
                            self.watch_chat(&name);
                            self.mining_watch(&name);
                        }
                    }
                    Err(err) => {
                        self.pending_stop.remove(&channel);
                        self.set_toast(format!("{channel}: {err}"));
                    }
                }
            }
            Event::UpdateChecked { result, silent } => match result {
                Ok(updater::UpdateCheck::UpToDate { latest, .. }) => {
                    self.update_state = UpdateState::UpToDate(latest);
                    if !silent {
                        self.set_toast(format!(
                            "Ya tienes la última versión ({})",
                            updater::display_version(updater::CURRENT_VERSION)
                        ));
                    }
                }
                Ok(updater::UpdateCheck::Available(release)) => {
                    let version = release.version_label.clone();
                    let can_auto = self.cfg.auto_update && release.download_url.is_some();
                    self.update_state = UpdateState::Available(release.clone());
                    if can_auto {
                        self.pending_auto_update = Some(release);
                        self.set_toast(format!(
                            "Nueva versión {version} encontrada; actualizando..."
                        ));
                    } else {
                        self.set_toast(format!(
                            "Nueva versión disponible: {version} (GitHub Releases)"
                        ));
                    }
                }
                Err(err) => {
                    self.update_state = UpdateState::Error(err.clone());
                    if !silent {
                        self.set_toast(format!("No se pudo comprobar actualizaciones: {err}"));
                    }
                }
            },
            Event::UpdateDownloaded(result) => match result {
                Ok(staged) => match updater::apply_and_restart(&staged) {
                    Ok(()) => {
                        self.save_state();
                        let _ = self.cfg.save();
                        self.shutdown();
                        std::process::exit(0);
                    }
                    Err(err) => {
                        let msg = format!("{err:#}");
                        self.update_state = UpdateState::Error(msg.clone());
                        self.set_toast(format!("No se pudo aplicar la actualización: {msg}"));
                    }
                },
                Err(err) => {
                    self.update_state = UpdateState::Error(err.clone());
                    self.set_toast(format!("Error al descargar actualización: {err}"));
                }
            },
        }
    }

    fn apply_row_action(&mut self, action: RowAction, ctx: &egui::Context) {
        match action {
            RowAction::Play(name) => {
                let audio_only = self
                    .queue
                    .iter()
                    .find(|channel| channel.name.eq_ignore_ascii_case(&name))
                    .map(|channel| channel.audio_only)
                    .unwrap_or(false);
                self.start_channel(ctx, &name, audio_only, None);
            }
            RowAction::PlayEmbedded(name) => {
                let audio_only = self
                    .queue
                    .iter()
                    .find(|channel| channel.name.eq_ignore_ascii_case(&name))
                    .map(|channel| channel.audio_only)
                    .unwrap_or(false);
                // Cambiar de modo: cerrar el reproductor externo de ese canal.
                if self.running.contains_key(&name) || self.launching.contains(&name) {
                    self.stop_channel(&name);
                }
                if !self.start_embedded(ctx, &name, audio_only) {
                    self.start_external(ctx, &name, audio_only, None);
                }
            }
            RowAction::PlayExternal(name) => {
                let audio_only = self
                    .queue
                    .iter()
                    .find(|channel| channel.name.eq_ignore_ascii_case(&name))
                    .map(|channel| channel.audio_only)
                    .unwrap_or(false);
                // Cambiar de modo: cerrar el reproductor integrado si es ese canal.
                if self.embedded_channel.as_deref() == Some(name.as_str()) {
                    self.stop_embedded();
                }
                if !self.running.contains_key(&name) {
                    self.start_external(ctx, &name, audio_only, None);
                }
            }
            RowAction::Stop(name) => self.stop_channel(&name),
            RowAction::ToggleAudio(name) => self.toggle_audio(ctx, &name),
            RowAction::ToggleFavorite(name) => self.toggle_favorite(&name),
            RowAction::Logs(name) => {
                self.logs_open = true;
                self.logs_channel = Some(name);
                self.refresh_logs();
            }
            RowAction::Remove(name) => self.remove_channel(&name),
            RowAction::AddToQueue(name, display) => {
                self.add_channel_named(&name, &display, ctx);
            }
            RowAction::OpenWeb(name) => {
                // Se delega en el navegador del sistema (la app no lleva WebView).
                let url = format!("https://twitch.tv/{}", name);
                let _ = std::process::Command::new("cmd")
                    .args(["/C", "start", "", &url])
                    .spawn();
            }
            RowAction::OpenChat(name) => {
                self.chat_join(&name);
                self.chat_channel = Some(name.to_lowercase());
                self.chat_follow = true;
                self.chat_panel = true;
            }
        }
    }

    // -- UI -----------------------------------------------------------------

    fn top_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = self.palette();
        egui::Panel::top("top_bar")
            .frame(
                egui::Frame::NONE
                    .fill(egui::Color32::TRANSPARENT)
                    .inner_margin(egui::Margin::symmetric(16, 9)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if let Some(logo) = &self.logo {
                        ui.add(
                            egui::Image::from_texture(logo)
                                .fit_to_exact_size(egui::vec2(30.0, 30.0))
                                .corner_radius(9),
                        );
                    }
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new("StreamHub")
                            .size(17.0)
                            .strong()
                            .color(p.text),
                    );

                    ui.add_space(16.0);
                    let search_width = (ui.available_width() - 250.0).clamp(180.0, 470.0);
                    let mut enter = false;
                    egui::Frame::NONE
                        .fill(p.surface2)
                        .corner_radius(egui::CornerRadius::same(17))
                        .stroke(egui::Stroke::new(1.0, p.border2))
                        .inner_margin(egui::Margin::symmetric(12, 4))
                        .show(ui, |ui| {
                            ui.set_width(search_width);
                            ui.set_height(26.0);
                            ui.horizontal_centered(|ui| {
                                let (icon_rect, _) = ui.allocate_exact_size(
                                    egui::vec2(14.0, 14.0),
                                    egui::Sense::hover(),
                                );
                                icons::paint(
                                    ui.painter(),
                                    icon_rect,
                                    Icon::Search,
                                    p.muted,
                                    egui::Color32::TRANSPARENT,
                                );
                                ui.add_space(4.0);
                                let response = ui.add_sized(
                                    [ui.available_width().max(60.0), 22.0],
                                    egui::TextEdit::singleline(&mut self.global_search)
                                        .frame(egui::Frame::NONE)
                                        .hint_text("Buscar canal, categoría o pegar URL..."),
                                );
                                enter = response.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            });
                        });
                    if enter {
                        let query = self.global_search.trim().to_string();
                        if !query.is_empty() {
                            self.global_search.clear();
                            self.run_global_search(ctx, &query);
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let live = self.live.values().filter(|info| info.live).count();
                        self.live_pill(ui, live, &p);
                        if icons::button(ui, Icon::Chat, self.chat_panel, true, "Chat", &p).clicked()
                        {
                            self.chat_panel = !self.chat_panel;
                            if self.chat_panel {
                                self.start_chat_thread();
                                // Al abrir el panel se une a los canales en cola
                                // y a los que se están reproduciendo.
                                let mut channels: Vec<String> = self
                                    .queue
                                    .iter()
                                    .map(|channel| channel.name.to_lowercase())
                                    .collect();
                                if let Some(channel) = &self.embedded_channel {
                                    channels.push(channel.clone());
                                }
                                channels.extend(self.running.keys().cloned());
                                channels.dedup();
                                for channel in channels {
                                    self.chat_join(&channel);
                                }
                                // La pestaña activa es la del stream en pantalla.
                                if let Some(channel) = &self.embedded_channel {
                                    self.chat_channel = Some(channel.clone());
                                    self.chat_follow = true;
                                } else if let Some((channel, _)) = self.running.iter().next() {
                                    self.chat_channel = Some(channel.clone());
                                    self.chat_follow = true;
                                }
                            }
                        }
                        if icons::button(ui, Icon::Refresh, false, true, "Refrescar", &p).clicked() {
                            self.refresh_current(ctx);
                        }
                        match &self.update_state {
                            UpdateState::Available(release) => {
                                let release = release.clone();
                                let label = format!("Actualizar {}", release.version_label);
                                if ui
                                    .add(
                                        egui::Button::new(
                                            egui::RichText::new(label)
                                                .size(11.0)
                                                .strong()
                                                .color(p.on_accent),
                                        )
                                        .fill(p.accent)
                                        .corner_radius(egui::CornerRadius::same(12)),
                                    )
                                    .on_hover_text("Descargar e instalar nueva versión desde GitHub")
                                    .clicked()
                                {
                                    self.spawn_update_download(ctx, release);
                                }
                            }
                            UpdateState::Downloading(ver) => {
                                ui.label(
                                    egui::RichText::new(format!("Actualizando {ver}..."))
                                        .size(11.0)
                                        .color(p.accent),
                                );
                            }
                            _ => {}
                        }
                    });
                });
            });
        // Delicate bottom border separating header from workspace.
        let top_rect = ui.min_rect();
        ui.painter().line_segment(
            [
                egui::pos2(top_rect.left(), top_rect.bottom()),
                egui::pos2(top_rect.right(), top_rect.bottom()),
            ],
            egui::Stroke::new(1.0, p.border),
        );
    }

    fn live_pill(&self, ui: &mut egui::Ui, live: usize, p: &Palette) {
        let text = format!("{live} en vivo");
        let font = egui::FontId::new(10.0, egui::FontFamily::Monospace);
        let galley = ui.painter().layout_no_wrap(text, font, p.muted);
        let width = galley.size().x + 28.0;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 24.0), egui::Sense::hover());
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(12), p.surface2);
        ui.painter().rect_stroke(
            rect,
            egui::CornerRadius::same(12),
            egui::Stroke::new(1.0, p.border),
            egui::StrokeKind::Inside,
        );
        let color = if live > 0 { p.green } else { p.dim };
        let dot = egui::pos2(rect.min.x + 11.0, rect.center().y);
        ui.painter().circle_filled(dot, 3.2, color);
        if live > 0 {
            let pulse = (ui.ctx().input(|i| i.time) as f32 * 2.2).sin() * 0.5 + 0.5;
            ui.painter()
                .circle_stroke(dot, 3.2 + pulse * 3.0, egui::Stroke::new(1.0, color.gamma_multiply(1.0 - pulse)));
        }
        ui.painter().galley(
            egui::pos2(rect.min.x + 19.0, rect.center().y - galley.size().y / 2.0),
            galley,
            p.muted,
        );
    }

    fn run_global_search(&mut self, ctx: &egui::Context, query: &str) {
        if let Some(channel) = parse_channel(query) {
            self.add_channel(&channel, ctx);
            return;
        }
        self.tab = Tab::Explore;
        self.search_query = query.to_string();
        self.spawn_search(ctx, query);
    }

    fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        // Keep the home screen clean when no players are active.
        if self.active_players() == 0 && self.embedded.is_none() {
            return;
        }
        let p = self.palette();
        egui::Panel::bottom("bottom_bar")
            .frame(
                egui::Frame::NONE
                    .fill(p.surface)
                    .stroke(egui::Stroke::new(1.0, p.border))
                    .inner_margin(egui::Margin::symmetric(16, 6)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(p.green, "●");
                    ui.label(
                        egui::RichText::new(format!(
                            "{} reproductor(es) activo(s)",
                            self.active_players()
                        ))
                        .size(11.5)
                        .color(p.text),
                    );
                    if self.active_players() > 0 && ui.small_button("Detener todos").clicked() {
                        self.stop_all();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(player) = &self.embedded {
                            let snap = player.snapshot();
                            ui.label(
                                egui::RichText::new(format!("Integrado: {}", snap.channel))
                                    .size(11.0)
                                    .color(p.muted),
                            );
                        }
                    });
                });
            });
    }

    fn side_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = self.palette();
        // Ancho animado: contraído (64) o expandido (arrastrable 180-320).
        let expanded = self.cfg.sidebar_width.clamp(180.0, 320.0);
        let target = if self.cfg.sidebar_collapsed {
            64.0
        } else {
            expanded
        };
        let width = anim::approach(ctx, egui::Id::unique("sidebar_width"), target, 0.22);
        if (width - target).abs() > 0.5 {
            ctx.request_repaint();
        }
        let collapsed = width < 150.0;
        let response = egui::Panel::left("side_bar")
            .resizable(false)
            .exact_size(width)
            .frame(
                egui::Frame::NONE
                    .fill(p.surface)
                    .inner_margin(egui::Margin::symmetric(
                        if collapsed { 8 } else { 12 },
                        12,
                    )),
            )
            .show(ui, |ui| {
                if collapsed {
                    self.side_bar_compact(ui, ctx);
                } else {
                    self.side_bar_expanded(ui, ctx);
                }
            });
        // Crisp vertical divider along the right edge of the sidebar.
        let side_rect = response.response.rect;
        ui.painter().line_segment(
            [
                egui::pos2(side_rect.right(), side_rect.top()),
                egui::pos2(side_rect.right(), side_rect.bottom()),
            ],
            egui::Stroke::new(1.0, p.border),
        );
        if !collapsed && !self.cfg.sidebar_collapsed {
            self.sidebar_drag_handle(ui, side_rect);
        } else {
            self.sidebar_drag = None;
        }
    }

    /// Borde arrastrable para cambiar el ancho del sidebar expandido.
    fn sidebar_drag_handle(&mut self, ui: &mut egui::Ui, panel: egui::Rect) {
        let handle = egui::Rect::from_min_size(
            egui::pos2(panel.right() - 3.0, panel.top()),
            egui::vec2(6.0, panel.height()),
        );
        egui::Area::new(egui::Id::unique("sidebar_drag_handle"))
            .fixed_pos(handle.min)
            .order(egui::Order::Middle)
            .show(ui.ctx(), |ui| {
                let (_, response) =
                    ui.allocate_exact_size(handle.size(), egui::Sense::drag());
                response.clone().on_hover_cursor(egui::CursorIcon::ResizeEast);
                if response.drag_started() {
                    let origin = response
                        .interact_pointer_pos()
                        .unwrap_or(handle.left_center());
                    self.sidebar_drag = Some((origin, self.cfg.sidebar_width));
                }
                if let Some((origin, base)) = self.sidebar_drag {
                    if let Some(pos) = ui.ctx().pointer_latest_pos() {
                        let max = (ui.ctx().viewport_rect().width() * 0.35).max(200.0);
                        self.cfg.sidebar_width =
                            (base + (pos.x - origin.x)).clamp(180.0, max.min(320.0));
                    }
                    ui.ctx().request_repaint();
                    if response.drag_stopped() {
                        self.sidebar_drag = None;
                        let _ = self.cfg.save();
                    }
                }
            });
    }

    /// Alterna entre el sidebar completo y el compacto (iconos).
    fn toggle_sidebar(&mut self) {
        self.cfg.sidebar_collapsed = !self.cfg.sidebar_collapsed;
        let _ = self.cfg.save();
    }

    /// Entrada escalonada de tarjetas tras recargar una lista: devuelve
    /// (opacidad, desplazamiento vertical). El retardo depende de la clave,
    /// así las tarjetas aparecen en cascada sin depender del orden.
    fn card_enter(&self, ui: &egui::Ui, key: &str) -> (f32, f32) {
        let elapsed = self.list_gen_at.elapsed().as_secs_f32();
        let hash = key
            .bytes()
            .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
        let delay = (hash % 9) as f32 * 0.02;
        let t = ((elapsed - delay) / 0.22).clamp(0.0, 1.0);
        if t < 1.0 {
            ui.ctx().request_repaint();
        }
        let eased = anim::ease(t, anim::Easing::EaseOut);
        (eased, (1.0 - eased) * 8.0)
    }

    /// Sidebar contraído: botones de icono con tooltip.
    fn side_bar_compact(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = self.palette();
        ui.vertical_centered(|ui| {
            if icons::button(ui, Icon::ChevronRight, false, true, "Expandir barra lateral", &p)
                .clicked()
            {
                self.toggle_sidebar();
            }
            ui.add_space(6.0);
            for (tab, label) in TABS {
                let selected = self.tab == tab;
                if icons::button(ui, tab.icon(), selected, true, label, &p).clicked() {
                    self.tab = tab;
                    if tab == Tab::Settings {
                        self.emote_cache_scanned = false;
                    }
                }
                ui.add_space(2.0);
            }
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(8.0);
            let has_queue = !self.queue.is_empty();
            if icons::button(ui, Icon::Play, false, has_queue, "Reproducir cola", &p).clicked() {
                self.start_all(ctx);
            }
            ui.add_space(4.0);
            if icons::button(ui, Icon::Grid, false, has_queue, "Cuadrícula", &p).clicked() {
                self.start_grid(ctx);
            }
            ui.add_space(4.0);
            let busy = !self.running.is_empty()
                || !self.launching.is_empty()
                || self.embedded.is_some();
            if icons::button(ui, Icon::Stop, false, busy, "Detener todo", &p).clicked() {
                self.stop_all();
            }
            ui.add_space(10.0);
            let dot = if self.has_creds() { p.green } else { p.gold };
            let (dot_rect, dot_res) =
                ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
            ui.painter().circle_filled(dot_rect.center(), 4.5, dot);
            dot_res.on_hover_text(if self.has_creds() {
                "Twitch conectado"
            } else {
                "Configura el token"
            });
        });
    }

    fn side_bar_expanded(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = self.palette();
        egui::Panel::bottom("side_footer")
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                ui.separator();
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let dot = if self.has_creds() { p.green } else { p.gold };
                    let (dot_rect, _) =
                        ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot_rect.center(), 3.5, dot);
                    ui.label(
                        egui::RichText::new(if self.has_creds() {
                            "Twitch conectado"
                        } else {
                            "Configura el token"
                        })
                        .size(11.0)
                        .color(p.muted),
                    );
                });
            });
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("NAVEGACIÓN")
                            .monospace()
                            .size(9.0)
                            .color(p.dim),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icons::button(
                            ui,
                            Icon::ChevronLeft,
                            false,
                            true,
                            "Contraer barra lateral",
                            &p,
                        )
                        .clicked()
                        {
                            self.toggle_sidebar();
                        }
                    });
                });
                ui.add_space(4.0);
                        for (tab, label) in TABS {
                    if self.tab_button(ui, tab, label, &p).clicked() {
                        self.tab = tab;
                        if tab == Tab::Settings {
                            // Al entrar en Ajustes se vuelven a medir las
                            // carpetas, por si el chat añadió emotes nuevos.
                            self.emote_cache_scanned = false;
                        }
                    }
                    ui.add_space(2.0);
                }

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(10.0);

                ui.label(
                    egui::RichText::new("AÑADIR CANAL")
                        .monospace()
                        .size(9.0)
                        .color(p.muted),
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let width = (ui.available_width() - 36.0).max(80.0);
                    let mut enter = false;
                    egui::Frame::NONE
                        .fill(p.surface2)
                        .corner_radius(egui::CornerRadius::same(14))
                        .stroke(egui::Stroke::new(1.0, p.border))
                        .inner_margin(egui::Margin::symmetric(10, 4))
                        .show(ui, |ui| {
                            ui.set_width((width - 20.0).max(60.0));
                            ui.set_height(24.0);
                            ui.horizontal_centered(|ui| {
                                let response = ui.add_sized(
                                    [ui.available_width().max(50.0), 20.0],
                                    egui::TextEdit::singleline(&mut self.new_channel)
                                        .frame(egui::Frame::NONE)
                                        .hint_text("canal o URL de Twitch"),
                                );
                                enter = response.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            });
                        });
                    if icons::button(ui, Icon::Plus, false, true, "Añadir a la cola", &p).clicked()
                        || enter
                    {
                        let name = std::mem::take(&mut self.new_channel);
                        self.add_channel(&name, ctx);
                    }
                });

                ui.add_space(10.0);
                let has_queue = !self.queue.is_empty();
                if icons::wide_button(
                    ui,
                    "Reproducir cola",
                    Icon::Play,
                    true,
                    has_queue,
                    &p,
                )
                .clicked()
                {
                    self.start_all(ctx);
                }
                ui.add_space(5.0);
                if icons::wide_button(ui, "Cuadrícula", Icon::Grid, false, has_queue, &p).clicked()
                {
                    self.start_grid(ctx);
                }
                ui.add_space(5.0);
                if icons::wide_button(
                    ui,
                    "Detener todo",
                    Icon::Stop,
                    false,
                    !self.running.is_empty()
                        || !self.launching.is_empty()
                        || self.embedded.is_some(),
                    &p,
                )
                .clicked()
                {
                    self.stop_all();
                }

                });
    }

    fn tab_player(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = self.palette();
        let Some(player) = self.embedded.clone() else {
            let rect = ui.available_rect_before_wrap();
            let painter = ui.painter();
            painter.text(
                egui::pos2(rect.center().x, rect.center().y - 26.0),
                egui::Align2::CENTER_CENTER,
                "No hay vídeo integrado",
                egui::FontId::proportional(17.0),
                p.text,
            );
            painter.text(
                egui::pos2(rect.center().x, rect.center().y + 2.0),
                egui::Align2::CENTER_CENTER,
                "Pulsa Reproducir en un canal para verlo aquí (backend glow).",
                egui::FontId::proportional(12.0),
                p.muted,
            );
            return;
        };
        let snap = player.snapshot();

        // El vídeo ocupa el panel superior; debajo mostramos la tarjeta de
        // información del stream (salvo en pantalla completa).
        let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        let avail = ui.available_size();
        let info_reserve = if fullscreen { 4.0 } else { 126.0 };
        let max_height = (avail.y - info_reserve).max(150.0);
        let mut width = avail.x.max(200.0);
        let mut height = width * 9.0 / 16.0;
        if height > max_height {
            height = max_height;
            width = height * 16.0 / 9.0;
        }
        let start = ui.cursor().min;
        let rect = egui::Rect::from_min_size(
            egui::pos2(start.x + ((avail.x - width) * 0.5).max(0.0), start.y),
            egui::vec2(width, height),
        );
        let video = ui.allocate_rect(rect, egui::Sense::click());

        // Doble clic: pantalla completa. Clic simple: pausa.
        if video.double_clicked() {
            let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
        } else if video.clicked() {
            player.toggle_pause();
            ctx.request_repaint();
        }

        let painter = ui.painter().clone();
        painter.rect_filled(rect, egui::CornerRadius::same(12), egui::Color32::BLACK);
        player.paint(ui, rect);

        // Visibilidad de los controles: siempre que no haya vídeo activo;
        // si lo hay, al mover el ratón sobre el vídeo y durante 2,5 s.
        let pointer = ctx.pointer_latest_pos();
        let inside = pointer.map(|pos| rect.contains(pos)).unwrap_or(false);
        let moving = ctx.input(|i| i.pointer.delta().length() > 1.0);
        if inside && moving {
            self.controls_at = Instant::now();
        }
        let show_controls = !snap.phase.is_active()
            || inside
            || self.controls_at.elapsed() < Duration::from_secs_f32(2.5);
        let controls_t =
            ctx.animate_bool_with_time(egui::Id::unique("player_controls"), show_controls, 0.25);

        // Cargando o con búfer: anillo giratorio sobre el vídeo.
        if matches!(
            snap.phase,
            embedded::Phase::Resolving | embedded::Phase::Loading | embedded::Phase::Buffering
        ) {
            spinner(&painter, rect.center(), 22.0, ctx.input(|i| i.time), p.text);
            ctx.request_repaint();
        }

        if !snap.phase.is_active() {
            painter.rect_filled(
                rect,
                egui::CornerRadius::same(12),
                egui::Color32::from_black_alpha(160),
            );
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                snap.phase.label(),
                egui::FontId::proportional(15.0),
                p.text,
            );
        }

        if controls_t > 0.01 {
            // Cabecera superpuesta arriba.
            let head = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), 38.0));
            painter.rect_filled(
                head,
                egui::CornerRadius::same(12),
                egui::Color32::from_black_alpha((120.0 * controls_t) as u8),
            );
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(head.shrink2(egui::vec2(12.0, 4.0)))
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            child.set_opacity(controls_t);
            child.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(snap.channel.to_uppercase())
                        .size(13.0)
                        .strong()
                        .color(p.text),
                );
                ui.add_space(6.0);
                let live = snap.phase.is_active();
                let color = if live { p.green } else { p.dim };
                ui.label(
                    egui::RichText::new(if live { "● EN DIRECTO" } else { "● INACTIVO" })
                        .monospace()
                        .size(9.0)
                        .color(color),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(snap.phase.label())
                            .size(10.5)
                            .color(p.muted),
                    );
                });
            });

            // Barra de controles superpuesta abajo, con degradado para
            // separarla del vídeo.
            let bar_h = 46.0;
            let bar = egui::Rect::from_min_size(
                egui::pos2(rect.left(), rect.bottom() - bar_h),
                egui::vec2(rect.width(), bar_h),
            );
            for i in 0..4 {
                let seg_h = bar_h / 4.0;
                let seg = egui::Rect::from_min_size(
                    egui::pos2(bar.min.x, bar.min.y + seg_h * i as f32),
                    egui::vec2(bar.width(), seg_h),
                );
                painter.rect_filled(
                    seg,
                    egui::CornerRadius::same(0),
                    egui::Color32::from_black_alpha((30.0 * (i as f32 + 1.0) * controls_t) as u8),
                );
            }
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(bar.shrink2(egui::vec2(12.0, 0.0)))
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            child.set_opacity(controls_t);
            // En ventanas estrechas se ocultan volumen y calidad para que los
            // botones esenciales sigan visibles.
            let narrow = child.available_width() < 560.0;
            child.horizontal(|ui| {
                let icon = if snap.paused { Icon::Play } else { Icon::Pause };
                let hint = if snap.paused { "Reanudar" } else { "Pausar" };
                if icons::button(ui, icon, false, true, hint, &p).clicked() {
                    player.toggle_pause();
                    ctx.request_repaint();
                }
                let mute_icon = if snap.muted { Icon::Mute } else { Icon::Volume };
                if icons::button(ui, mute_icon, snap.muted, true, "Silenciar", &p).clicked() {
                    player.toggle_mute();
                    ctx.request_repaint();
                }
                if !narrow {
                    let mut volume = snap.volume;
                    let response = ui.add_sized(
                        [120.0, 24.0],
                        egui::Slider::new(&mut volume, 0..=100).show_value(false),
                    );
                    if response.changed() {
                        player.set_volume(volume);
                    }
                    ui.label(
                        egui::RichText::new(format!("{}%", snap.volume))
                            .size(10.5)
                            .color(p.muted),
                    );

                    ui.add_space(4.0);
                    let selected = embedded::quality_label(&snap.quality).to_string();
                    egui::ComboBox::from_id_salt("embedded_quality")
                        .selected_text(selected)
                        .width(88.0)
                        .show_ui(ui, |ui| {
                            for quality in embedded::QUALITIES {
                                if ui
                                    .selectable_label(
                                        snap.quality == quality,
                                        embedded::quality_label(quality),
                                    )
                                    .clicked()
                                {
                                    player.set_quality(ctx, quality);
                                    ctx.request_repaint();
                                }
                            }
                        });
                }

                ui.add_space(2.0);
                if icons::button(
                    ui,
                    Icon::Video,
                    !snap.audio_only,
                    true,
                    if snap.audio_only {
                        "Activar vídeo"
                    } else {
                        "Solo audio"
                    },
                    &p,
                )
                .clicked()
                {
                    player.set_video(snap.audio_only);
                    ctx.request_repaint();
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icons::button(ui, Icon::Close, false, true, "Cerrar reproductor", &p)
                        .clicked()
                    {
                        self.stop_embedded();
                    }
                    if icons::button(
                        ui,
                        Icon::External,
                        false,
                        true,
                        "Abrir en el reproductor externo",
                        &p,
                    )
                    .clicked()
                    {
                        self.open_embedded_external(ctx);
                    }
                    if icons::button(ui, Icon::Refresh, false, true, "Reintentar", &p).clicked() {
                        player.reload(ctx);
                        ctx.request_repaint();
                    }
                });
            });
        }

        if !fullscreen {
            ui.add_space(8.0);
            self.player_stream_info(ui, &snap.channel, &p);
        }
    }

    /// Twitch-style stream metadata card displayed below the embedded player.
    fn player_stream_info(&mut self, ui: &mut egui::Ui, channel_name: &str, p: &Palette) {
        let key = channel_name.to_lowercase();
        if !self.user_avatars.contains_key(&key) {
            self.fetch_avatars(&[key.clone()]);
        }
        let display = self.display_for(&key);
        let live_info = self.live.get(&key).cloned();
        let is_live = live_info.as_ref().map(|info| info.live).unwrap_or(false);
        let viewers = live_info.as_ref().map(|info| info.viewers).unwrap_or(0);
        let title = live_info
            .as_ref()
            .map(|info| info.title.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                if is_live {
                    "Emisión en directo".to_string()
                } else {
                    "Sin información del directo".to_string()
                }
            });
        let game = live_info
            .as_ref()
            .map(|info| info.game.trim().to_string())
            .unwrap_or_default();
        let is_followed = self
            .followed
            .iter()
            .any(|ch| ch.broadcaster_login.eq_ignore_ascii_case(&key));
        let is_fav = self.is_favorite(&key);
        let uptime = self.embedded_started.map(|started| started.elapsed());

        egui::Frame::NONE
            .fill(p.surface)
            .stroke(egui::Stroke::new(1.0, p.border))
            .corner_radius(egui::CornerRadius::same(14))
            .inner_margin(egui::Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;

                    // Channel avatar with subtle live ring.
                    let avatar_box = ui.vertical(|ui| {
                        let pos = ui.cursor().min;
                        self.avatar(ui, &key, 46.0);
                        if is_live {
                            let center = pos + egui::vec2(23.0, 23.0);
                            ui.painter().circle_stroke(
                                center,
                                25.0,
                                egui::Stroke::new(2.0, p.red),
                            );
                        }
                    });
                    let _ = avatar_box;

                    // Middle: channel name, title, and category pill.
                    let right_reserve = 220.0;
                    let text_width = (ui.available_width() - right_reserve).max(160.0);
                    ui.vertical(|ui| {
                        ui.set_max_width(text_width);
                        ui.spacing_mut().item_spacing.y = 3.0;
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            ui.label(
                                egui::RichText::new(&display)
                                    .size(15.0)
                                    .strong()
                                    .color(p.text),
                            );
                            if is_live {
                                egui::Frame::NONE
                                    .fill(theme::with_alpha(p.red, 0.16))
                                    .corner_radius(egui::CornerRadius::same(8))
                                    .inner_margin(egui::Margin::symmetric(7, 2))
                                    .show(ui, |ui| {
                                        ui.label(
                                            egui::RichText::new("EN VIVO")
                                                .size(9.0)
                                                .strong()
                                                .color(p.red),
                                        );
                                    });
                            }
                        });
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&title)
                                    .size(12.5)
                                    .color(p.text),
                            )
                            .truncate(),
                        );
                        if !game.is_empty() {
                            egui::Frame::NONE
                                .fill(p.accent_weak)
                                .corner_radius(egui::CornerRadius::same(10))
                                .inner_margin(egui::Margin::symmetric(8, 2))
                                .show(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new(&game)
                                            .size(10.5)
                                            .color(p.accent),
                                    );
                                });
                        }
                    });

                    // Right: Follow / Favorite buttons on top, viewers + uptime below.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 6.0;
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    if icons::button(
                                        ui,
                                        Icon::Heart,
                                        is_fav,
                                        true,
                                        if is_fav {
                                            "Quitar de favoritos"
                                        } else {
                                            "Añadir a favoritos"
                                        },
                                        p,
                                    )
                                    .clicked()
                                    {
                                        self.toggle_favorite(&key);
                                    }

                                    let follow_label = if is_followed {
                                        "Siguiendo"
                                    } else {
                                        "Seguir"
                                    };
                                    let follow_btn = egui::Button::new(
                                        egui::RichText::new(follow_label)
                                            .size(11.5)
                                            .strong()
                                            .color(if is_followed { p.text } else { p.on_accent }),
                                    )
                                    .fill(if is_followed { p.surface2 } else { p.accent })
                                    .stroke(egui::Stroke::new(
                                        1.0,
                                        if is_followed { p.border2 } else { p.accent },
                                    ))
                                    .corner_radius(egui::CornerRadius::same(14))
                                    .min_size(egui::vec2(86.0, 28.0));
                                    if ui
                                        .add(follow_btn)
                                        .on_hover_text(format!(
                                            "Abrir twitch.tv/{key} para gestionar seguimiento"
                                        ))
                                        .clicked()
                                    {
                                        let _ = std::process::Command::new("cmd")
                                            .args(["/C", "start", "", &format!("https://twitch.tv/{key}")])
                                            .spawn();
                                        self.set_toast(format!("Abriendo twitch.tv/{key}"));
                                    }
                                },
                            );

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.spacing_mut().item_spacing.x = 10.0;
                                    if let Some(&pts) = self.mining_points.get(&key) {
                                        let pts_btn = egui::Button::new(
                                            egui::RichText::new(format!("{} pts", format_count(pts)))
                                                .size(11.0)
                                                .strong()
                                                .color(p.gold),
                                        )
                                        .fill(p.surface2)
                                        .stroke(egui::Stroke::new(1.0, p.border2))
                                        .corner_radius(egui::CornerRadius::same(12));
                                        if ui
                                            .add(pts_btn)
                                            .on_hover_text("Puntos del canal (clic para ver y canjear recompensas)")
                                            .clicked()
                                        {
                                            if !self.chat_panel {
                                                self.chat_panel = true;
                                            }
                                            self.chat_channel = Some(key.clone());
                                            self.emote_picker = false;
                                            self.points_popup_open = !self.points_popup_open;
                                            self.mining_refresh_channel(&key);
                                        }
                                    }
                                    if let Some(dur) = uptime {
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 4.0;
                                            let (icon_rect, _) = ui.allocate_exact_size(
                                                egui::vec2(12.0, 12.0),
                                                egui::Sense::hover(),
                                            );
                                            icons::paint(
                                                ui.painter(),
                                                icon_rect,
                                                Icon::Clock,
                                                p.muted,
                                                egui::Color32::TRANSPARENT,
                                            );
                                            ui.label(
                                                egui::RichText::new(format_duration(dur))
                                                    .size(11.0)
                                                    .monospace()
                                                    .color(p.muted),
                                            );
                                        });
                                    }
                                    if is_live {
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 4.0;
                                            let (icon_rect, _) = ui.allocate_exact_size(
                                                egui::vec2(13.0, 13.0),
                                                egui::Sense::hover(),
                                            );
                                            icons::paint(
                                                ui.painter(),
                                                icon_rect,
                                                Icon::Users,
                                                p.red,
                                                egui::Color32::TRANSPARENT,
                                            );
                                            ui.label(
                                                egui::RichText::new(format_count(viewers))
                                                    .size(11.5)
                                                    .strong()
                                                    .color(p.red),
                                            );
                                        });
                                    }
                                },
                            );
                        });
                    });
                });
            });
    }

    fn tab_button(
        &mut self,
        ui: &mut egui::Ui,
        tab: Tab,
        label: &str,
        p: &Palette,
    ) -> egui::Response {
        let active = self.tab == tab;
        let width = ui.available_width().max(80.0);
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(width, 38.0), egui::Sense::click());
        if ui.is_rect_visible(rect) {
            let hover_t = ui.ctx().animate_bool_with_time(
                egui::Id::unique(("nav_hover", label)),
                response.hovered(),
                0.15,
            );
            let active_t = ui.ctx().animate_bool_with_time(
                egui::Id::unique(("nav_active", label)),
                active,
                0.18,
            );
            let active_eased = anim::ease(active_t, anim::Easing::BackOut).clamp(0.0, 1.15);
            let base = theme::lerp_color(egui::Color32::TRANSPARENT, p.surface2, hover_t * 0.85);
            let bg = theme::lerp_color(base, p.accent_weak, active_t);
            ui.painter()
                .rect_filled(rect, egui::CornerRadius::same(12), bg);
            if active_t > 0.01 {
                let half_h = (9.5 * active_eased).min(12.0);
                let bar = egui::Rect::from_min_max(
                    egui::pos2(rect.min.x + 3.0, rect.center().y - half_h),
                    egui::pos2(rect.min.x + 6.5, rect.center().y + half_h),
                );
                ui.painter()
                    .rect_filled(bar, egui::CornerRadius::same(3), p.accent);
            }
            let fg = if active {
                p.accent
            } else if response.hovered() {
                p.text
            } else {
                p.muted
            };
            // Cápsula duotone pastel visible alrededor del icono (Cozy Pastel Café).
            let badge_center = egui::pos2(rect.min.x + 23.0, rect.center().y);
            let badge_bg = if active {
                theme::with_alpha(p.accent, 0.26)
            } else {
                theme::with_alpha(p.accent, 0.08 + 0.10 * hover_t)
            };
            let badge_rect = egui::Rect::from_center_size(badge_center, egui::vec2(26.0, 26.0));
            ui.painter()
                .rect_filled(badge_rect, egui::CornerRadius::same(9), badge_bg);
            ui.painter().rect_stroke(
                badge_rect,
                egui::CornerRadius::same(9),
                egui::Stroke::new(
                    1.0,
                    theme::with_alpha(p.accent, if active { 0.48 } else { 0.12 + 0.18 * hover_t }),
                ),
                egui::StrokeKind::Inside,
            );
            let icon_rect = egui::Rect::from_center_size(badge_center, egui::vec2(14.0, 14.0));
            icons::paint(ui.painter(), icon_rect, tab.icon(), fg, badge_bg);
            ui.painter().text(
                egui::pos2(rect.min.x + 44.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::new(13.0, egui::FontFamily::Proportional),
                fg,
            );
            if let Some(count) = self.tab_count(tab) {
                let text = count.to_string();
                let font = egui::FontId::new(9.5, egui::FontFamily::Monospace);
                let galley = ui.painter().layout_no_wrap(text, font, fg);
                let chip_w = (galley.size().x + 12.0).max(24.0);
                let chip = egui::Rect::from_center_size(
                    egui::pos2(rect.max.x - 10.0 - chip_w / 2.0, rect.center().y),
                    egui::vec2(chip_w, 18.0),
                );
                let chip_bg = if active { p.accent_mid } else { p.surface2 };
                ui.painter()
                    .rect_filled(chip, egui::CornerRadius::same(9), chip_bg);
                ui.painter().galley(
                    egui::pos2(
                        chip.center().x - galley.size().x / 2.0,
                        chip.center().y - galley.size().y / 2.0,
                    ),
                    galley,
                    if active { p.accent } else { p.muted },
                );
            }
        }
        response
    }

    fn tab_queue(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let live = self.live.values().filter(|info| info.live).count();
        let meta = format!(
            "{} canal(es) · {} en vivo · {} reproductor(es)",
            self.queue.len(),
            live,
            self.active_players()
        );
        self.content_header(ui, "Cola de reproducción", &meta, true, ctx);

        if self.mining_running || !self.mining_points.is_empty() || !self.mining_chests.is_empty() {
            self.mining_box(ui, ctx);
            ui.add_space(10.0);
        }

        if self.queue.is_empty() {
            self.empty_state(
                ui,
                Icon::Tv,
                "La cola está vacía",
                "Añade un canal desde la barra lateral o explora las categorías en vivo.",
            );
            return;
        }

        let channels = self.queue.clone();
        let mut actions = Vec::new();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                match self.cfg.view {
                    ViewMode::List => {
                        for channel in &channels {
                            self.channel_card(ui, channel, RowKind::Queue, true, &mut actions);
                            ui.add_space(6.0);
                        }
                    }
                    ViewMode::Grid => {
                        let columns = grid_columns(ui.available_width(), 300.0);
                        for chunk in channels.chunks(columns) {
                            ui.columns(columns, |cols| {
                                for (index, channel) in chunk.iter().enumerate() {
                                    self.channel_card(
                                        &mut cols[index],
                                        channel,
                                        RowKind::Queue,
                                        false,
                                        &mut actions,
                                    );
                                }
                            });
                            ui.add_space(8.0);
                        }
                    }
                }
            });
        for action in actions {
            self.apply_row_action(action, ctx);
        }
    }

    fn tab_favorites(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let live = self
            .cfg
            .favorites
            .iter()
            .filter(|name| {
                self.live
                    .get(&name.to_lowercase())
                    .map(|info| info.live)
                    .unwrap_or(false)
            })
            .count();
        let meta = format!(
            "{} favorito(s) · {} en vivo",
            self.cfg.favorites.len(),
            live
        );
        self.content_header(ui, "Favoritos", &meta, true, ctx);

        let favorites = self.cfg.favorites.clone();
        if favorites.is_empty() {
            self.empty_state(
                ui,
                Icon::Heart,
                "Aún no tienes favoritos",
                "Marca canales con el corazón desde la cola o desde Explorar.",
            );
            return;
        }

        let mut actions = Vec::new();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                match self.cfg.view {
                    ViewMode::List => {
                        for name in &favorites {
                            let channel = Channel {
                                name: name.clone(),
                                display: self.display_for(name),
                                audio_only: false,
                            };
                            self.channel_card(
                                ui,
                                &channel,
                                RowKind::Favorite,
                                true,
                                &mut actions,
                            );
                            ui.add_space(6.0);
                        }
                    }
                    ViewMode::Grid => {
                        let columns = grid_columns(ui.available_width(), 300.0);
                        for chunk in favorites.chunks(columns) {
                            ui.columns(columns, |cols| {
                                for (index, name) in chunk.iter().enumerate() {
                                    let channel = Channel {
                                        name: name.clone(),
                                        display: self.display_for(name),
                                        audio_only: false,
                                    };
                                    self.channel_card(
                                        &mut cols[index],
                                        &channel,
                                        RowKind::Favorite,
                                        false,
                                        &mut actions,
                                    );
                                }
                            });
                            ui.add_space(8.0);
                        }
                    }
                }
            });
        for action in actions {
            self.apply_row_action(action, ctx);
        }
    }

    fn tab_followed(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let account = self
            .followed_user
            .as_ref()
            .map(|user| user.display_name.clone())
            .unwrap_or_else(|| "sin cuenta".to_string());
        let live_count = self
            .followed
            .iter()
            .filter(|channel| self.is_followed_live(channel))
            .count();
        let meta = format!(
            "{} seguidos · {} en vivo · {account}",
            self.followed.len(),
            live_count
        );
        self.content_header(ui, "Seguidos", &meta, false, ctx);

        if !self.has_creds() {
            self.empty_state(
                ui,
                Icon::Users,
                "Configura tu cuenta de Twitch",
                "Añade el Client ID y el token en Ajustes para cargar tus canales seguidos.",
            );
            return;
        }

        let p = self.palette();

        if !self.followed_loaded && !self.busy_followed {
            self.spawn_followed(ctx);
        }

        if self.busy_followed {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(16.0, 16.0),
                    egui::Sense::hover(),
                );
                spinner(ui.painter(), rect.center(), 8.0, ctx.input(|i| i.time), p.accent);
                ui.weak("Consultando seguidos...");
            });
            ui.add_space(8.0);
        }

        if self.followed.is_empty() {
            if !self.busy_followed {
                ui.add_space(12.0);
                if icons::wide_button(ui, "Reintentar", Icon::Refresh, true, true, &self.palette()).clicked() {
                    self.spawn_followed(ctx);
                }
                ui.add_space(10.0);
                ui.weak("No se encontraron canales seguidos. Pulsa Reintentar o el icono de refrescar.");
            }
            return;
        }

        let total = self.followed.len();
        let offline_count = total.saturating_sub(live_count);
        ui.horizontal(|ui| {
            if filter_chip(
                ui,
                &format!("Todos {total}"),
                self.followed_filter == FollowFilter::All,
                &p,
            )
            .clicked()
            {
                self.followed_filter = FollowFilter::All;
            }
            if filter_chip(
                ui,
                &format!("En vivo {live_count}"),
                self.followed_filter == FollowFilter::Live,
                &p,
            )
            .clicked()
            {
                self.followed_filter = FollowFilter::Live;
            }
            if filter_chip(
                ui,
                &format!("Sin emisión {offline_count}"),
                self.followed_filter == FollowFilter::Offline,
                &p,
            )
            .clicked()
            {
                self.followed_filter = FollowFilter::Offline;
            }
        });
        ui.add_space(8.0);

        let followed: Vec<FollowedChannel> = self
            .followed
            .iter()
            .filter(|channel| match self.followed_filter {
                FollowFilter::All => true,
                FollowFilter::Live => self.is_followed_live(channel),
                FollowFilter::Offline => !self.is_followed_live(channel),
            })
            .cloned()
            .collect();

        if followed.is_empty() {
            ui.add_space(12.0);
            ui.weak(match self.followed_filter {
                FollowFilter::All => "No hay canales seguidos.",
                FollowFilter::Live => "Ninguno de tus seguidos está en directo ahora mismo.",
                FollowFilter::Offline => "Todos tus seguidos están en directo.",
            });
            return;
        }

        let mut actions = Vec::new();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for channel in &followed {
                    self.followed_card(ui, channel, &mut actions);
                    ui.add_space(6.0);
                }
            });
        for action in actions {
            self.apply_row_action(action, ctx);
        }
    }

    fn is_followed_live(&self, channel: &FollowedChannel) -> bool {
        self.live
            .get(&channel.broadcaster_login.to_lowercase())
            .map(|info| info.live)
            .unwrap_or(false)
    }

    fn tab_explore(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let meta = if let Some(game) = &self.explore_game {
            let page_len = self
                .explore_pages
                .get(self.explore_page)
                .map(Vec::len)
                .unwrap_or(0);
            format!(
                "{} · Pág. {} · {} en vivo",
                game.name,
                self.explore_page + 1,
                page_len
            )
        } else {
            let page_len = self
                .games_pages
                .get(self.games_page)
                .map(Vec::len)
                .unwrap_or(0);
            format!(
                "Pág. {} · {} categoría(s) · {} resultado(s)",
                self.games_page + 1,
                page_len,
                self.search_results.len()
            )
        };
        self.content_header(ui, "Explorar", &meta, false, ctx);

        let scroll_id = (
            "explore_scroll",
            self.explore_game.as_ref().map(|g| g.id.clone()),
            if self.explore_game.is_some() {
                self.explore_page
            } else {
                self.games_page
            },
        );
        egui::ScrollArea::vertical()
            .id_salt(scroll_id)
            .auto_shrink([false, false])
            .show(ui, |ui| self.explore_body(ui, ctx));
    }

    fn explore_body(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = self.palette();
        ui.horizontal(|ui| {
            let width = (ui.available_width() - 110.0).max(160.0);
            let response = ui.add_sized(
                [width, 34.0],
                egui::TextEdit::singleline(&mut self.search_query)
                    .hint_text("Buscar canales en vivo..."),
            );
            let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if icons::wide_button(
                ui,
                "Buscar",
                Icon::Search,
                false,
                true,
                &self.palette(),
            )
            .clicked()
                || enter
            {
                let query = self.search_query.trim().to_string();
                if !query.is_empty() {
                    self.spawn_search(ctx, &query);
                }
            }
            if self.busy_search {
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(16.0, 16.0),
                    egui::Sense::hover(),
                );
                spinner(ui.painter(), rect.center(), 8.0, ctx.input(|i| i.time), p.accent);
            }
        });
        ui.add_space(8.0);

        if !self.search_results.is_empty() {
            section_title(ui, "Resultados de búsqueda", self.search_results.len(), &self.palette());
            let results = self.search_results.clone();
            let mut actions = Vec::new();
            let columns = grid_columns(ui.available_width(), 300.0);
            for chunk in results.chunks(columns) {
                ui.columns(columns, |cols| {
                    for (index, result) in chunk.iter().enumerate() {
                        let login = result.broadcaster_login.to_lowercase();
                        let display = if result.display_name.is_empty() {
                            result.broadcaster_login.clone()
                        } else {
                            result.display_name.clone()
                        };
                        self.stream_card(
                            &mut cols[index],
                            &login,
                            &display,
                            &result.game_name,
                            &result.title,
                            result.viewer_count,
                            &result.thumbnail_url,
                            result.is_live,
                            &mut actions,
                        );
                    }
                });
                ui.add_space(8.0);
            }
            for action in actions {
                self.apply_row_action(action, ctx);
            }
            ui.add_space(6.0);
        }

        if self.has_creds() && self.games_pages.is_empty() && !self.busy_games && !self.games_loaded
        {
            self.spawn_games(ctx, 0);
        }

        if let Some(game) = self.explore_game.clone() {
            let page_streams = self
                .explore_pages
                .get(self.explore_page)
                .cloned()
                .unwrap_or_default();
            ui.horizontal(|ui| {
                if icons::wide_button(
                    ui,
                    "Categorías",
                    Icon::ChevronLeft,
                    false,
                    true,
                    &p,
                )
                .clicked()
                {
                    self.explore_game = None;
                    self.explore_pages.clear();
                    self.explore_page = 0;
                    self.explore_cursor = None;
                    self.busy_explore = false;
                }
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(format!("En vivo en {}", game.name))
                        .size(14.0)
                        .strong()
                        .color(p.text),
                );
                ui.label(
                    egui::RichText::new(format!(
                        "Pág. {} · {}",
                        self.explore_page + 1,
                        page_streams.len()
                    ))
                    .size(11.5)
                    .color(p.muted),
                );
            });
            ui.add_space(8.0);

            if self.busy_explore && self.explore_pages.is_empty() {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(16.0, 16.0),
                        egui::Sense::hover(),
                    );
                    spinner(ui.painter(), rect.center(), 8.0, ctx.input(|i| i.time), p.accent);
                    ui.weak("Cargando streams de la categoría...");
                });
                return;
            }

            if page_streams.is_empty() {
                ui.add_space(8.0);
                ui.weak("No hay streams en vivo en esta categoría ahora mismo.");
                return;
            }

            let mut actions = Vec::new();
            let columns = grid_columns(ui.available_width(), 300.0);
            for chunk in page_streams.chunks(columns) {
                ui.columns(columns, |cols| {
                    for (index, stream) in chunk.iter().enumerate() {
                        let login = stream.user_login.to_lowercase();
                        let display = if stream.user_name.is_empty() {
                            stream.user_login.clone()
                        } else {
                            stream.user_name.clone()
                        };
                        self.stream_card(
                            &mut cols[index],
                            &login,
                            &display,
                            &stream.game_name,
                            &stream.title,
                            stream.viewer_count,
                            &stream.thumbnail_url,
                            true,
                            &mut actions,
                        );
                    }
                });
                ui.add_space(8.0);
            }
            for action in actions {
                self.apply_row_action(action, ctx);
            }

            ui.add_space(6.0);
            if let Some(target_page) = pagination_bar(
                ui,
                ctx,
                self.explore_page,
                self.explore_pages.len(),
                self.explore_cursor.is_some(),
                self.busy_explore,
                &p,
            ) {
                self.spawn_explore(ctx, target_page);
            }
            return;
        }

        if self.games_pages.is_empty() {
            if self.busy_games {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(16.0, 16.0),
                        egui::Sense::hover(),
                    );
                    spinner(ui.painter(), rect.center(), 8.0, ctx.input(|i| i.time), p.accent);
                    ui.weak("Cargando categorías...");
                });
            } else {
                self.empty_state(
                    ui,
                    Icon::Gamepad,
                    "Explora por categoría",
                    "Carga las categorías populares de Twitch y descubre qué se está emitiendo.",
                );
                if icons::wide_button(
                    ui,
                    "Cargar categorías",
                    Icon::Refresh,
                    true,
                    true,
                    &self.palette(),
                )
                .clicked()
                {
                    self.reload_games(ctx);
                }
            }
            return;
        }

        let page_games = self
            .games_pages
            .get(self.games_page)
            .cloned()
            .unwrap_or_default();
        section_title(
            ui,
            &format!("Categorías populares · Pág. {}", self.games_page + 1),
            page_games.len(),
            &self.palette(),
        );
        let selected_id = self.explore_game.as_ref().map(|game| game.id.clone());
        let mut chosen: Option<Game> = None;
        let columns = grid_columns(ui.available_width(), 150.0);
        for chunk in page_games.chunks(columns) {
            ui.columns(columns, |cols| {
                for (index, game) in chunk.iter().enumerate() {
                    let selected = selected_id.as_deref() == Some(game.id.as_str());
                    if self.game_card(&mut cols[index], game, selected) {
                        chosen = Some(game.clone());
                    }
                }
            });
            ui.add_space(8.0);
        }
        ui.add_space(6.0);
        if let Some(target_page) = pagination_bar(
            ui,
            ctx,
            self.games_page,
            self.games_pages.len(),
            self.games_cursor.is_some(),
            self.busy_games,
            &p,
        ) {
            self.spawn_games(ctx, target_page);
        }
        if let Some(game) = chosen {
            self.explore_game = Some(game);
            self.reload_explore(ctx);
        }
    }

    fn tab_settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.content_header(
            ui,
            "Ajustes",
            "Apariencia, reproducción y credenciales",
            false,
            ctx,
        );
        let p = self.palette();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                settings_section(ui, "APARIENCIA", true, &p, |ui| {
                    ui.label(
                        egui::RichText::new("Tema")
                            .size(11.0)
                            .color(p.muted),
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        for mode in Theme::ALL {
                            let selected = self.cfg.theme == mode;
                            let swatch = theme::palette(mode, config::accent_color(mode.default_accent()));
                            let (rect, response) = ui
                                .allocate_exact_size(egui::vec2(84.0, 44.0), egui::Sense::click());
                            let hover_t = ui.ctx().animate_bool_with_time(
                                egui::Id::unique(("theme_swatch", mode.label())),
                                response.hovered(),
                                0.12,
                            );
                            let scale = 1.0 + 0.06 * hover_t;
                            let rect = egui::Rect::from_center_size(
                                rect.center(),
                                egui::vec2(rect.width() * scale, rect.height() * scale),
                            );
                            // Muestra de la paleta: fondo, superficie y acento.
                            ui.painter().rect_filled(
                                rect,
                                egui::CornerRadius::same(9),
                                swatch.bg,
                            );
                            ui.painter().rect_filled(
                                egui::Rect::from_min_size(
                                    rect.min + egui::vec2(6.0, 6.0),
                                    egui::vec2(rect.width() - 12.0, 8.0),
                                ),
                                egui::CornerRadius::same(4),
                                swatch.surface,
                            );
                            ui.painter().rect_filled(
                                egui::Rect::from_min_size(
                                    rect.min + egui::vec2(6.0, 18.0),
                                    egui::vec2(rect.width() - 12.0, 8.0),
                                ),
                                egui::CornerRadius::same(4),
                                swatch.surface2,
                            );
                            ui.painter().rect_filled(
                                egui::Rect::from_min_size(
                                    rect.min + egui::vec2(6.0, 30.0),
                                    egui::vec2((rect.width() - 12.0) * 0.45, 6.0),
                                ),
                                egui::CornerRadius::same(3),
                                swatch.accent,
                            );
                            ui.painter().rect_stroke(
                                rect,
                                egui::CornerRadius::same(9),
                                egui::Stroke::new(
                                    if selected { 2.0 } else { 1.0 },
                                    if selected { swatch.accent } else { p.border },
                                ),
                                egui::StrokeKind::Inside,
                            );
                            ui.painter().text(
                                egui::pos2(rect.center().x, rect.max.y + 10.0),
                                egui::Align2::CENTER_CENTER,
                                mode.label(),
                                egui::FontId::new(10.5, egui::FontFamily::Proportional),
                                if selected { p.accent } else { p.muted },
                            );
                            if response.clicked() {
                                // Si el acento es el de la paleta anterior, se adopta
                                // el de la nueva (como en la web).
                                if self
                                    .cfg
                                    .accent
                                    .eq_ignore_ascii_case(self.cfg.theme.default_accent())
                                {
                                    self.cfg.accent = mode.default_accent().to_string();
                                }
                                self.cfg.theme = mode;
                                self.apply_theme(ui.ctx());
                                let _ = self.cfg.save();
                            }
                        }
                    });
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new("Color de acento rápido")
                            .size(11.0)
                            .color(p.muted),
                    );
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        for hex in [
                            "#ff8fab", "#7ce8c4", "#c8a2ff", "#ffae8a", "#8ec5ff", "#e0553a",
                            "#9d78f5", "#2eb86a", "#f0a830",
                        ] {
                            let color = config::accent_color(hex);
                            let (rect, response) =
                                ui.allocate_exact_size(egui::vec2(28.0, 24.0), egui::Sense::click());
                            let hover_t = ui.ctx().animate_bool_with_time(
                                ui.make_persistent_id(("accent_swatch", hex)),
                                response.hovered(),
                                0.12,
                            );
                            let scale = 1.0 + 0.1 * hover_t;
                            let rect = egui::Rect::from_center_size(
                                rect.center(),
                                egui::vec2(rect.width() * scale, rect.height() * scale),
                            );
                            ui.painter()
                                .rect_filled(rect, egui::CornerRadius::same(8), color);
                            let active_hex = self
                                .cfg
                                .custom_colors
                                .accent
                                .as_deref()
                                .unwrap_or(&self.cfg.accent);
                            if active_hex.eq_ignore_ascii_case(hex) {
                                ui.painter().rect_stroke(
                                    rect.expand(2.0),
                                    egui::CornerRadius::same(10),
                                    egui::Stroke::new(2.0, p.text),
                                    egui::StrokeKind::Outside,
                                );
                            }
                            if response.clicked() {
                                self.cfg.accent = hex.to_string();
                                self.cfg.custom_colors.accent = Some(hex.to_string());
                                self.apply_theme(ui.ctx());
                                let _ = self.cfg.save();
                            }
                        }
                        let mut accent = self
                            .cfg
                            .custom_colors
                            .accent
                            .clone()
                            .unwrap_or_else(|| self.cfg.accent.clone());
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut accent)
                                    .hint_text("#ff8fab")
                                    .desired_width(86.0),
                            )
                            .changed()
                        {
                            self.cfg.accent = accent.clone();
                            if config::parse_hex_color(&accent).is_some() {
                                self.cfg.custom_colors.accent = Some(accent);
                                self.apply_theme(ui.ctx());
                            }
                        }
                    });

                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Colores personalizados (independientes)")
                                .size(11.5)
                                .strong()
                                .color(p.text),
                        );
                        if !self.cfg.custom_colors.is_empty()
                            && ui
                                .button(
                                    egui::RichText::new("Restablecer colores del tema")
                                        .size(10.5)
                                        .color(p.muted),
                                )
                                .clicked()
                        {
                            self.cfg.custom_colors.clear();
                            self.cfg.accent = self.cfg.theme.default_accent().to_string();
                            self.apply_theme(ui.ctx());
                            let _ = self.cfg.save();
                        }
                    });
                    ui.add_space(6.0);
                    let mut colors_changed = false;
                    let base_p = theme::palette(
                        self.cfg.theme,
                        config::accent_color(self.cfg.theme.default_accent()),
                    );
                    egui::Grid::new("custom_colors_grid")
                        .num_columns(2)
                        .spacing([18.0, 8.0])
                        .show(ui, |ui| {
                            let slots: [(&str, egui::Color32, &mut Option<String>); 6] = [
                                ("Fondo principal", base_p.bg, &mut self.cfg.custom_colors.bg),
                                ("Tarjetas y paneles", base_p.surface, &mut self.cfg.custom_colors.surface),
                                ("Acento principal", base_p.accent, &mut self.cfg.custom_colors.accent),
                                ("Indicador En vivo", base_p.red, &mut self.cfg.custom_colors.live),
                                ("Secundario / Menta", base_p.green, &mut self.cfg.custom_colors.secondary),
                                ("Texto principal", base_p.text, &mut self.cfg.custom_colors.text),
                            ];
                            for (idx, (label, fallback, slot)) in slots.into_iter().enumerate() {
                                ui.horizontal(|ui| {
                                    let mut col = slot
                                        .as_deref()
                                        .and_then(config::parse_hex_color)
                                        .unwrap_or(fallback);
                                    if egui::color_picker::color_edit_button_srgba(
                                        ui,
                                        &mut col,
                                        egui::color_picker::Alpha::Opaque,
                                    )
                                    .changed()
                                    {
                                        *slot = Some(config::color_to_hex(col));
                                        colors_changed = true;
                                    }
                                    ui.label(egui::RichText::new(label).size(11.5).color(p.text));
                                    if slot.is_some()
                                        && ui
                                            .small_button("↺")
                                            .on_hover_text("Usar color del tema")
                                            .clicked()
                                    {
                                        *slot = None;
                                        colors_changed = true;
                                    }
                                });
                                if idx % 2 == 1 {
                                    ui.end_row();
                                }
                            }
                        });
                    if colors_changed {
                        if let Some(acc) = &self.cfg.custom_colors.accent {
                            self.cfg.accent = acc.clone();
                        }
                        self.apply_theme(ui.ctx());
                        let _ = self.cfg.save();
                    }

                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        ui.label("Vista por defecto");
                        for view in [ViewMode::List, ViewMode::Grid] {
                            ui.selectable_value(&mut self.cfg.view, view, view.label());
                        }
                    });
                });

                ui.add_space(10.0);
                settings_section(ui, "RENDERIZADO", true, &p, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Backend gráfico");
                        let previous = self.cfg.renderer;
                        egui::ComboBox::from_id_salt("render_backend")
                            .selected_text(self.cfg.renderer.label())
                            .show_ui(ui, |ui| {
                                for backend in config::RenderBackend::ALL {
                                    ui.selectable_value(
                                        &mut self.cfg.renderer,
                                        backend,
                                        backend.label(),
                                    );
                                }
                            });
                        if previous != self.cfg.renderer {
                            let _ = self.cfg.save();
                            self.set_toast("Backend guardado: reinicia para aplicarlo");
                        }
                        ui.label(
                            egui::RichText::new(format!(
                                "· en uso: {}",
                                self.renderer_active.label()
                            ))
                            .size(10.5)
                            .color(p.dim),
                        );
                    });
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(
                            "glow usa OpenGL y suele arrancar más rápido; wgpu usa DirectX 12/Vulkan. \
                             El cambio se aplica al reiniciar. Si wgpu falla, se vuelve a glow automáticamente.",
                        )
                        .size(10.5)
                        .color(p.muted),
                    );
                    ui.add_space(6.0);
                    if icons::wide_button(
                        ui,
                        "Reiniciar ahora",
                        Icon::Refresh,
                        true,
                        true,
                        &p,
                    )
                    .clicked()
                    {
                        self.restart_app();
                    }
                });

                ui.add_space(10.0);
                settings_section(ui, "REPRODUCCIÓN", true, &p, |ui| {
                    egui::Grid::new("settings_grid")
                        .num_columns(2)
                        .spacing([18.0, 10.0])
                        .show(ui, |ui| {
                            ui.label("Reproductor");
                            ui.horizontal(|ui| {
                                for player in [Player::Mpv, Player::Vlc] {
                                    ui.selectable_value(
                                        &mut self.cfg.player,
                                        player,
                                        player.label(),
                                    );
                                }
                            });
                            ui.end_row();

                            ui.label("Integrado");
                            ui.horizontal(|ui| {
                                if ui
                                    .checkbox(
                                        &mut self.cfg.embedded,
                                        "Reproducir el stream dentro de la app",
                                    )
                                    .changed()
                                {
                                    let _ = self.cfg.save();
                                }
                                if self.cfg.embedded
                                    && self.renderer_active != config::RenderBackend::Glow
                                {
                                    ui.label(
                                        egui::RichText::new("requiere el backend glow")
                                            .size(10.0)
                                            .color(p.red),
                                    );
                                }
                            });
                            ui.end_row();

                            ui.label("Diseño");
                            ui.horizontal(|ui| {
                                if ui
                                    .checkbox(
                                        &mut self.cfg.compact,
                                        "Tarjetas compactas",
                                    )
                                    .changed()
                                {
                                    let _ = self.cfg.save();
                                }
                                ui.label(
                                    egui::RichText::new("más información en menos espacio")
                                        .size(9.0)
                                        .color(p.muted),
                                );
                            });
                            ui.end_row();

                            ui.label("Chatterino");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.cfg.chatterino)
                                    .hint_text("Ruta a Chatterino2.exe (opcional)")
                                    .desired_width(320.0),
                            );
                            ui.end_row();

                            ui.label("Calidad");
                            egui::ComboBox::from_id_salt("quality")
                                .selected_text(self.cfg.quality.as_str())
                                .show_ui(ui, |ui| {
                                    for quality in [
                                        "best", "1080p60", "720p60", "720p", "480p", "360p",
                                        "worst", "audio_only",
                                    ] {
                                        ui.selectable_value(
                                            &mut self.cfg.quality,
                                            quality.to_string(),
                                            quality,
                                        );
                                    }
                                });
                            ui.end_row();

                            ui.label("Ancho de ventana");
                            ui.add(
                                egui::DragValue::new(&mut self.cfg.width)
                                    .range(320..=7680)
                                    .suffix(" px"),
                            );
                            ui.end_row();

                            ui.label("Alto de ventana");
                            ui.add(
                                egui::DragValue::new(&mut self.cfg.height)
                                    .range(240..=4320)
                                    .suffix(" px"),
                            );
                            ui.end_row();

                            ui.label("Aceleración por hardware");
                            ui.checkbox(&mut self.cfg.hwaccel, "hwdec=auto-safe (mpv)");
                            ui.end_row();

                            ui.label("GPU preferida");
                            egui::ComboBox::from_id_salt("gpu")
                                .selected_text(self.cfg.hw_gpu.as_str())
                                .show_ui(ui, |ui| {
                                    for gpu in ["nvidia", "amd", "intel"] {
                                        ui.selectable_value(
                                            &mut self.cfg.hw_gpu,
                                            gpu.to_string(),
                                            gpu,
                                        );
                                    }
                                });
                            ui.end_row();
                        });
                });

                ui.add_space(10.0);
                settings_section(ui, "RUTAS DE HERRAMIENTAS", false, &p, |ui| {
                    egui::Grid::new("paths_grid")
                        .num_columns(2)
                        .spacing([18.0, 10.0])
                        .show(ui, |ui| {
                            path_row(ui, "mpv", &mut self.cfg.mpv);
                            path_row(ui, "VLC", &mut self.cfg.vlc);
                            path_row(ui, "Streamlink", &mut self.cfg.streamlink);
                            path_row(ui, "FFmpeg (opcional)", &mut self.cfg.ffmpeg);
                        });
                });

                ui.add_space(10.0);
                settings_section(ui, "TWITCH API", true, &p, |ui| {
                    egui::Grid::new("twitch_grid")
                        .num_columns(2)
                        .spacing([18.0, 10.0])
                        .show(ui, |ui| {
                            ui.label("Client ID");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.cfg.client_id)
                                    .hint_text("Client ID de tu aplicación de Twitch")
                                    .desired_width(380.0),
                            );
                            ui.end_row();

                            ui.label("Token");
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.token_edit)
                                        .password(!self.token_visible)
                                        .desired_width(320.0),
                                );
                                ui.checkbox(&mut self.token_visible, "Mostrar");
                                if ui.button("Guardar").clicked() {
                                    self.save_token();
                                }
                                if ui.button("Eliminar").clicked() {
                                    self.delete_token();
                                }
                            });
                            ui.end_row();
                        });
                    ui.add_space(4.0);
                    ui.weak(
                        "El token se guarda en el Administrador de credenciales de Windows, nunca en config.json.",
                    );
                });

                ui.add_space(10.0);
                settings_section(ui, "CACHÉ DE EMOTES", false, &p, |ui| {
                    self.emote_cache_section(ui, &p);
                });

                ui.add_space(10.0);
                settings_section(ui, "ACTUALIZACIONES", true, &p, |ui| {
                    let current = updater::display_version(updater::CURRENT_VERSION);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            egui::RichText::new(format!("Versión actual: {current}"))
                                .strong()
                                .color(p.text),
                        );
                        ui.weak(format!(
                            "({}/{})",
                            updater::REPO_OWNER,
                            updater::REPO_NAME
                        ));
                    });
                    ui.add_space(4.0);
                    if ui
                        .checkbox(
                            &mut self.cfg.auto_update,
                            "Actualizar automáticamente al detectar una nueva release en GitHub",
                        )
                        .changed()
                    {
                        let _ = self.cfg.save();
                    }
                    ui.add_space(6.0);
                    let mut start_download: Option<updater::ReleaseInfo> = None;
                    let mut check_now = false;
                    match &self.update_state {
                        UpdateState::Idle => {
                            ui.weak("Comprueba GitHub Releases para buscar nuevas versiones.");
                        }
                        UpdateState::Checking => {
                            ui.horizontal(|ui| {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(14.0, 14.0),
                                    egui::Sense::hover(),
                                );
                                spinner(
                                    ui.painter(),
                                    rect.center(),
                                    7.0,
                                    ctx.input(|i| i.time),
                                    p.accent,
                                );
                                ui.weak("Comprobando releases en GitHub...");
                            });
                        }
                        UpdateState::UpToDate(latest) => {
                            let msg = match latest {
                                Some(ver) => format!("Al día (última release publicada: {ver})."),
                                None => "Al día (aún no hay releases publicadas en el repositorio)."
                                    .to_string(),
                            };
                            ui.colored_label(p.green, msg);
                        }
                        UpdateState::Available(release) => {
                            ui.colored_label(
                                p.accent,
                                format!("Nueva versión disponible: {}", release.version_label),
                            );
                            if !release.notes.is_empty() {
                                let preview: String =
                                    release.notes.chars().take(240).collect();
                                ui.weak(preview);
                            }
                            ui.add_space(4.0);
                            if icons::wide_button(
                                ui,
                                &format!("Actualizar a {}", release.version_label),
                                Icon::Refresh,
                                true,
                                true,
                                &p,
                            )
                            .clicked()
                            {
                                start_download = Some(release.clone());
                            }
                        }
                        UpdateState::Downloading(ver) => {
                            ui.horizontal(|ui| {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(14.0, 14.0),
                                    egui::Sense::hover(),
                                );
                                spinner(
                                    ui.painter(),
                                    rect.center(),
                                    7.0,
                                    ctx.input(|i| i.time),
                                    p.accent,
                                );
                                ui.colored_label(
                                    p.accent,
                                    format!("Descargando e instalando {ver}..."),
                                );
                            });
                        }
                        UpdateState::Error(err) => {
                            ui.colored_label(p.red, format!("Error: {err}"));
                        }
                    }
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        let busy = matches!(
                            self.update_state,
                            UpdateState::Checking | UpdateState::Downloading(_)
                        );
                        if ui
                            .add_enabled(!busy, egui::Button::new("Buscar actualizaciones"))
                            .clicked()
                        {
                            check_now = true;
                        }
                        if ui.button("Ver releases en GitHub").clicked() {
                            open_url(updater::RELEASES_PAGE);
                        }
                    });
                    if let Some(release) = start_download {
                        self.spawn_update_download(ctx, release);
                    } else if check_now {
                        self.spawn_update_check(ctx, false);
                    }
                });

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if icons::wide_button(
                        ui,
                        "Guardar configuración",
                        Icon::Check,
                        true,
                        true,
                        &p,
                    )
                    .clicked()
                    {
                        self.save_config(ctx);
                    }
                });
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Comprobar herramientas").clicked() {
                        self.check_tools();
                    }
                    if ui.button("Abrir carpeta de datos").clicked() {
                        open_path(&config::data_dir());
                    }
                    if ui.button("Abrir registros").clicked() {
                        open_path(&config::logs_dir());
                    }
                });

                ui.add_space(10.0);
                ui.weak(format!("Datos: {}", config::data_dir().display()));
                ui.weak(format!(
                    "Versión {} ({}) — Rust + egui (sin navegador ni motor web)",
                    updater::display_version(updater::CURRENT_VERSION),
                    env!("CARGO_PKG_VERSION")
                ));
            });
    }

    fn content_header(
        &mut self,
        ui: &mut egui::Ui,
        title: &str,
        meta: &str,
        show_view: bool,
        ctx: &egui::Context,
    ) {
        let p = self.palette();
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new(title)
                        .size(20.0)
                        .strong()
                        .color(p.text),
                );
                ui.label(egui::RichText::new(meta).size(11.0).color(p.muted));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icons::button(ui, Icon::Refresh, false, true, "Refrescar", &p).clicked() {
                    self.refresh_current(ctx);
                }
                if show_view {
                    ui.add_space(4.0);
                    let is_grid = self.cfg.view == ViewMode::Grid;
                    if icons::button(ui, Icon::Grid, is_grid, true, "Vista de cuadrícula", &p)
                        .clicked()
                    {
                        self.cfg.view = ViewMode::Grid;
                        let _ = self.cfg.save();
                    }
                    if icons::button(ui, Icon::List, !is_grid, true, "Vista de lista", &p)
                        .clicked()
                    {
                        self.cfg.view = ViewMode::List;
                        let _ = self.cfg.save();
                    }
                }
            });
        });
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);
    }

    fn empty_state(&self, ui: &mut egui::Ui, icon: Icon, title: &str, text: &str) {
        let p = self.palette();
        let height = (ui.available_height() - 84.0).max(120.0);
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), height),
            egui::Sense::hover(),
        );
        let center = egui::pos2(rect.center().x, rect.center().y - 20.0);
        icons::paint(
            ui.painter(),
            egui::Rect::from_center_size(center - egui::vec2(0.0, 40.0), egui::vec2(46.0, 46.0)),
            icon,
            p.dim,
            p.bg,
        );
        ui.painter().text(
            center + egui::vec2(0.0, 8.0),
            egui::Align2::CENTER_CENTER,
            title,
            egui::FontId::new(17.0, egui::FontFamily::Proportional),
            p.muted,
        );
        ui.painter().text(
            center + egui::vec2(0.0, 32.0),
            egui::Align2::CENTER_CENTER,
            text,
            egui::FontId::new(12.0, egui::FontFamily::Proportional),
            p.dim,
        );
    }

    fn channel_card(
        &mut self,
        ui: &mut egui::Ui,
        channel: &Channel,
        kind: RowKind,
        list: bool,
        actions: &mut Vec<RowAction>,
    ) {
        let width = ui.available_width().max(140.0);
        let compact = self.cfg.compact;
        let thumb_h = (width - 20.0) * 9.0 / 16.0;
        let list_thumb_w = 196.0_f32.min((width - 20.0) * 0.42);
        let list_thumb_h = list_thumb_w * 9.0 / 16.0;
        let height = if list {
            (list_thumb_h + if compact { 14.0 } else { 20.0 }).max(if compact { 80.0 } else { 120.0 })
        } else {
            thumb_h + if compact { 56.0 } else { 126.0 }
        };
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
        if !ui.is_rect_visible(rect) {
            return;
        }
        let p = self.palette();
        let key = channel.name.to_lowercase();
        let live = self.live.get(&key).cloned();
        let is_live = live.as_ref().map(|info| info.live).unwrap_or(false);
        let running = self.is_playing(&key);
        let launching = self.launching.contains(&key);
        let favorite = self.is_favorite(&key);
        let uptime = self
            .running
            .get(&key)
            .map(|stream| stream.started.elapsed())
            .or_else(|| {
                if self.embedded_channel.as_deref() == Some(key.as_str()) {
                    self.embedded_started.map(|started| started.elapsed())
                } else {
                    None
                }
            });

        let (enter_t, enter_y) = self.card_enter(ui, &key);
        let prev_opacity = ui.opacity();
        ui.set_opacity(prev_opacity * enter_t);
        let hover_t = ui.ctx().animate_bool_with_time(
            ui.make_persistent_id(("channel_card", key.as_str())),
            response.hovered(),
            0.15,
        );
        // Elevación y sombra al hover (como `.dash-card:hover` de la web).
        let lift = -if compact { 1.5 } else { 3.0 } * hover_t;
        let rect = rect.translate(egui::vec2(0.0, lift + enter_y));
        if hover_t > 0.01 {
            anim::shadow(
                ui.painter(),
                rect,
                if compact { 8.0 } else { 14.0 },
                [0, 4],
                12,
                egui::Color32::from_black_alpha((36.0 * hover_t) as u8),
            );
        }
        let fill = theme::lerp_color(p.surface, p.surface3, hover_t * 0.6);
        let base_border = if is_live { p.border2 } else { p.border };
        let border = if running {
            p.accent
        } else {
            theme::lerp_color(base_border, p.accent, hover_t * 0.8)
        };
        let card_r = if compact { 10 } else { 16 };
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(card_r), fill);
        ui.painter().rect_stroke(
            rect,
            egui::CornerRadius::same(if compact { 8 } else { 14 }),
            egui::Stroke::new(if running { 1.4 } else { 1.0 }, border),
            egui::StrokeKind::Inside,
        );
        if running {
            let bar = egui::Rect::from_min_size(
                rect.min + egui::vec2(8.0, 0.0),
                egui::vec2(rect.width() - 16.0, if compact { 2.0 } else { 3.0 }),
            );
            ui.painter()
                .rect_filled(bar, egui::CornerRadius::same(1), p.accent);
        }

        let inner_margin = if compact { 6.0 } else { 10.0 };
        let inner = rect.shrink(inner_margin);
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );

        if list {
            child.horizontal(|ui| {
                let thumb_w = 196.0_f32.min(inner.width() * 0.42);
                let thumb_h = thumb_w * 9.0 / 16.0;
                self.thumbnail(ui, egui::vec2(thumb_w, thumb_h), &live, &channel.name, true);
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width());
                    // Nombre del canal: siempre en una línea, con truncate si es largo.
                    ui.horizontal(|ui| {
                        self.avatar(ui, &channel.name, if compact { 20.0 } else { 26.0 });
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(channel.label())
                                    .size(if compact { 13.0 } else { 15.0 })
                                    .strong()
                                    .color(p.text),
                            )
                            .truncate()
                            .sense(egui::Sense::hover()),
                        );
                        if channel.audio_only {
                            badge(ui, "AUDIO", p.blue, p.accent_weak);
                        }
                        if running {
                            badge(ui, "EN REPRODUCCIÓN", p.green, p.surface2);
                            if let Some(uptime) = uptime {
                                ui.weak(format_duration(uptime));
                            }
                        } else if launching {
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(14.0, 14.0),
                                egui::Sense::hover(),
                            );
                            spinner(ui.painter(), rect.center(), 7.0, ui.ctx().input(|i| i.time), p.accent);
                            ui.weak("iniciando...");
                        }
                    });
                    ui.add_space(2.0);
                    if let Some(info) = &live {
                        if info.live {
                            if !info.title.is_empty() {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(&info.title)
                                            .size(11.0)
                                            .color(p.text),
                                    )
                                    .truncate(),
                                );
                            }
                            ui.label(
                                egui::RichText::new(format!(
                                    "{}  ·  {} esp.",
                                    if info.game.is_empty() {
                                        "Sin categoría"
                                    } else {
                                        info.game.as_str()
                                    },
                                    format_count(info.viewers)
                                ))
                                .size(10.0)
                                .color(p.muted),
                            );
                        } else {
                            ui.label(
                                egui::RichText::new("Sin emisión")
                                    .size(10.5)
                                    .color(p.dim),
                            );
                        }
                    } else {
                        ui.label(
                            egui::RichText::new("Estado desconocido")
                                .size(10.5)
                                .color(p.dim),
                        );
                    }
                    ui.add_space(4.0);
                    self.action_buttons(
                        ui,
                        &key,
                        channel.label(),
                        kind,
                        running,
                        launching,
                        favorite,
                        channel.audio_only,
                        actions,
                        &p,
                    );
                });
            });
        } else {
            let thumb_width = child.available_width();
            self.thumbnail(
                &mut child,
                egui::vec2(thumb_width, thumb_h),
                &live,
                &channel.name,
                true,
            );
            child.add_space(4.0);
            child.horizontal(|ui| {
                let avatar_size = if compact { 20.0 } else { 24.0 };
                self.avatar(ui, &channel.name, avatar_size);
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(channel.label())
                            .size(if compact { 13.0 } else { 14.0 })
                            .strong()
                            .color(p.text),
                    )
                    .truncate(),
                );
                if running {
                    ui.add_space(4.0);
                    ui.colored_label(p.green, "●");
                } else if launching {
                    ui.add_space(4.0);
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(14.0, 14.0),
                        egui::Sense::hover(),
                    );
                    spinner(ui.painter(), rect.center(), 7.0, ui.ctx().input(|i| i.time), p.accent);
                }
            });
            if let Some(info) = &live {
                if info.live {
                    child.add(
                        egui::Label::new(
                            egui::RichText::new(if info.title.is_empty() {
                                "Sin título"
                            } else {
                                info.title.as_str()
                            })
                            .size(if compact { 11.0 } else { 11.5 })
                            .color(p.muted),
                        )
                        .truncate(),
                    );
                    child.label(
                        egui::RichText::new(format!(
                            "{} · {} esp.",
                            if info.game.is_empty() {
                                "Sin categoría"
                            } else {
                                info.game.as_str()
                            },
                            format_count(info.viewers)
                        ))
                        .size(if compact { 10.0 } else { 10.5 })
                        .color(p.dim),
                    );
                } else {
                    child.label(
                        egui::RichText::new("Sin emisión")
                            .size(if compact { 10.0 } else { 11.0 })
                            .color(p.dim),
                    );
                }
            }
            child.add_space(2.0);
            self.action_buttons(
                &mut child,
                &key,
                channel.label(),
                kind,
                running,
                launching,
                favorite,
                channel.audio_only,
                actions,
                &p,
            );
        }

        if response.double_clicked() && !running && !launching {
            actions.push(RowAction::Play(key.clone()));
        }
        ui.set_opacity(prev_opacity);
    }

    #[allow(clippy::too_many_arguments)]
    fn action_buttons(
        &mut self,
        ui: &mut egui::Ui,
        key: &str,
        display: &str,
        kind: RowKind,
        running: bool,
        launching: bool,
        favorite: bool,
        audio_only: bool,
        actions: &mut Vec<RowAction>,
        p: &Palette,
    ) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 4.0);
            if running {
                if icons::button(ui, Icon::Stop, true, true, "Detener reproducción", p).clicked() {
                    actions.push(RowAction::Stop(key.to_string()));
                }
            } else if icons::button(ui, Icon::Play, false, !launching, "Reproducir canal", p)
                .clicked()
            {
                actions.push(RowAction::Play(key.to_string()));
            }

            if icons::button(ui, Icon::Chat, false, true, "Abrir chat", p).clicked() {
                actions.push(RowAction::OpenChat(key.to_string()));
            }

            let fav_tip = if favorite {
                "Quitar de favoritos"
            } else {
                "Añadir a favoritos"
            };
            if icons::button(ui, Icon::Heart, favorite, true, fav_tip, p).clicked() {
                actions.push(RowAction::ToggleFavorite(key.to_string()));
            }

            if matches!(kind, RowKind::Favorite | RowKind::Followed)
                && icons::button(ui, Icon::Plus, false, true, "Añadir a la cola", p).clicked()
            {
                actions.push(RowAction::AddToQueue(
                    key.to_string(),
                    display.to_string(),
                ));
            }

            // Menú limpio de acciones secundarias (sin recargar la tarjeta).
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button(
                    egui::RichText::new("...").size(13.0).strong().color(p.muted),
                    |ui| {
                        ui.set_min_width(175.0);
                        if ui.button("Reproducir dentro de la app").clicked() {
                            actions.push(RowAction::PlayEmbedded(key.to_string()));
                            ui.close();
                        }
                        if ui
                            .button(format!("Abrir en {}", self.cfg.player.label()))
                            .clicked()
                        {
                            actions.push(RowAction::PlayExternal(key.to_string()));
                            ui.close();
                        }
                        if matches!(kind, RowKind::Queue | RowKind::Favorite) {
                            let label = if audio_only {
                                "Activar vídeo"
                            } else {
                                "Modo solo audio"
                            };
                            if ui.button(label).clicked() {
                                actions.push(RowAction::ToggleAudio(key.to_string()));
                                ui.close();
                            }
                        }
                        ui.separator();
                        if ui.button("Abrir en navegador (Twitch)").clicked() {
                            actions.push(RowAction::OpenWeb(key.to_string()));
                            ui.close();
                        }
                        if ui.button("Ver registros").clicked() {
                            actions.push(RowAction::Logs(key.to_string()));
                            ui.close();
                        }
                        if matches!(kind, RowKind::Queue) {
                            ui.separator();
                            if ui
                                .button(egui::RichText::new("Quitar de la cola").color(p.red))
                                .clicked()
                            {
                                actions.push(RowAction::Remove(key.to_string()));
                                ui.close();
                            }
                        }
                    },
                );
            });
        });
    }

    fn thumbnail(
        &mut self,
        ui: &mut egui::Ui,
        size: egui::Vec2,
        live: &Option<LiveInfo>,
        name: &str,
        badges: bool,
    ) {
        let p = self.palette();
        let (rect, thumb_resp) = ui.allocate_exact_size(size, egui::Sense::hover());
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(12), p.surface3);
        let mut has_image = false;
        let mut loading_live = false;
        if let Some(info) = live {
            if info.live && !info.thumbnail_url.is_empty() {
                if let Some(texture) = self.images.get(ui.ctx(), &info.thumbnail_url, 400, 225) {
                    egui::Image::from_texture((texture.id(), size))
                        .corner_radius(12.0)
                        .paint_at(ui, rect);
                    has_image = true;
                } else {
                    loading_live = true;
                }
            }
        }
        // Botón Play circular flotante al pasar el ratón sobre la miniatura.
        if has_image {
            let play_hov = ui.ctx().animate_bool_with_time(
                ui.make_persistent_id(("thumb_play", name)),
                thumb_resp.hovered(),
                0.16,
            );
            if play_hov > 0.02 {
                let eased = anim::ease(play_hov, anim::Easing::BackOut).clamp(0.0, 1.12);
                let r = 18.0 * eased;
                ui.painter().circle_filled(
                    rect.center(),
                    r,
                    theme::with_alpha(p.accent, 0.88 * play_hov),
                );
                let icon_rect = egui::Rect::from_center_size(
                    rect.center() + egui::vec2(1.0, 0.0),
                    egui::vec2(r * 1.05, r * 1.05),
                );
                icons::paint(
                    ui.painter(),
                    icon_rect,
                    Icon::Play,
                    theme::with_alpha(p.on_accent, play_hov),
                    egui::Color32::TRANSPARENT,
                );
            }
        }
        if !has_image {
            let base_accent = theme::name_color(name, self.cfg.theme);
            let tint = theme::lerp_color(p.surface2, base_accent, 0.20);
            ui.painter()
                .rect_filled(rect, egui::CornerRadius::same(12), tint);
            ui.painter().rect_stroke(
                rect,
                egui::CornerRadius::same(12),
                egui::Stroke::new(1.0, theme::with_alpha(base_accent, 0.26)),
                egui::StrokeKind::Inside,
            );
            if loading_live {
                // Shimmer sutil mientras llega la miniatura en vivo.
                let phase = ((ui.input(|i| i.time) as f32) * 1.1).fract();
                let band_w = (rect.width() * 0.38).max(28.0);
                let x = rect.min.x - band_w + phase * (rect.width() + band_w * 2.0);
                let band = egui::Rect::from_min_max(
                    egui::pos2(x.max(rect.min.x), rect.min.y),
                    egui::pos2((x + band_w).min(rect.max.x), rect.max.y),
                );
                if band.width() > 1.0 {
                    ui.painter().rect_filled(
                        band,
                        egui::CornerRadius::same(8),
                        theme::with_alpha(egui::Color32::WHITE, 0.08),
                    );
                }
                ui.ctx().request_repaint_after(Duration::from_millis(33));
            }
            let badge_r = (size.y * 0.20).clamp(14.0, 24.0);
            ui.painter().circle_filled(
                rect.center(),
                badge_r,
                theme::with_alpha(egui::Color32::BLACK, 0.28),
            );
            ui.painter().circle_stroke(
                rect.center(),
                badge_r,
                egui::Stroke::new(1.0, theme::with_alpha(egui::Color32::WHITE, 0.22)),
            );
            let initials = initials(name);
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                initials,
                egui::FontId::new((size.y * 0.20).clamp(12.0, 20.0), egui::FontFamily::Proportional),
                theme::with_alpha(egui::Color32::WHITE, 0.90),
            );
        }
        if !badges {
            return;
        }
        if let Some(info) = live {
            if info.live {
                // El badge "EN VIVO" late suavemente (mejora sobre la web).
                let pulse = anim::pulse(ui.input(|i| i.time), 2.0);
                icons::chip_paint(
                    ui.painter(),
                    rect.min + egui::vec2(6.0, 6.0),
                    "EN VIVO",
                    egui::Color32::WHITE,
                    theme::with_alpha(p.red, pulse),
                );
                if info.viewers > 0 {
                    let text = format!("{} esp.", format_count(info.viewers));
                    let font = egui::FontId::new(9.0, egui::FontFamily::Monospace);
                    let galley = ui
                        .painter()
                        .layout_no_wrap(text.clone(), font, egui::Color32::WHITE);
                    let w = galley.size().x + 12.0;
                    let pos = egui::pos2(
                        rect.max.x - w - 6.0,
                        rect.max.y - galley.size().y - 11.0,
                    );
                    icons::chip_paint(
                        ui.painter(),
                        pos,
                        &text,
                        egui::Color32::WHITE,
                        egui::Color32::from_black_alpha(170),
                    );
                }
            }
        }
    }

    fn avatar(&mut self, ui: &mut egui::Ui, name: &str, size: f32) {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
        // Imagen real del canal (Helix) cuando ya está descargada.
        if let Some(url) = self.user_avatars.get(&name.to_lowercase()) {
            let url = url.clone();
            if let Some(texture) = self.images.get_cached(ui.ctx(), &url, 24, 24, None) {
                ui.put(
                    rect,
                    egui::Image::from_texture(&texture)
                        .fit_to_exact_size(egui::vec2(size, size))
                        .corner_radius((size / 2.0) as u8),
                );
                return;
            }
        }
        let color = theme::name_color(name, self.cfg.theme);
        ui.painter().circle_filled(rect.center(), size / 2.0, color);
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            initials(name),
            egui::FontId::new(size * 0.38, egui::FontFamily::Proportional),
            egui::Color32::WHITE,
        );
    }

    fn followed_card(
        &mut self,
        ui: &mut egui::Ui,
        channel: &FollowedChannel,
        actions: &mut Vec<RowAction>,
    ) {
        let width = ui.available_width().max(140.0);
        let height = 118.0;
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
        if !ui.is_rect_visible(rect) {
            return;
        }
        let p = self.palette();
        let login = channel.broadcaster_login.to_lowercase();
        let display = if channel.broadcaster_name.is_empty() {
            channel.broadcaster_login.clone()
        } else {
            channel.broadcaster_name.clone()
        };
        let live = self.live.get(&login).cloned();
        let favorite = self.is_favorite(&login);

        let (enter_t, enter_y) = self.card_enter(ui, &login);
        let prev_opacity = ui.opacity();
        ui.set_opacity(prev_opacity * enter_t);
        let hover_t = ui.ctx().animate_bool_with_time(
            ui.make_persistent_id(("followed_card", login.as_str())),
            response.hovered(),
            0.15,
        );
        let lift = -3.0 * hover_t;
        let rect = rect.translate(egui::vec2(0.0, lift + enter_y));
        if hover_t > 0.01 {
            anim::shadow(
                ui.painter(),
                rect,
                14.0,
                [0, 6],
                16,
                egui::Color32::from_black_alpha((36.0 * hover_t) as u8),
            );
        }
        let fill = theme::lerp_color(p.surface, p.surface3, hover_t * 0.6);
        let border = theme::lerp_color(p.border, p.accent, hover_t * 0.8);
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(14), fill);
        ui.painter().rect_stroke(
            rect,
            egui::CornerRadius::same(14),
            egui::Stroke::new(1.0, border),
            egui::StrokeKind::Inside,
        );

        let inner = rect.shrink(10.0);
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        self.thumbnail(
            &mut child,
            egui::vec2(124.0, 69.75),
            &live,
            &login,
            true,
        );
        child.add_space(10.0);
        let mut info = child.new_child(
            egui::UiBuilder::new()
                .max_rect(child.available_rect_before_wrap())
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        info.horizontal(|ui| {
            self.avatar(ui, &login, 26.0);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(&display)
                        .size(15.0)
                        .strong()
                        .color(p.text),
                )
                .truncate(),
            );
            if let Some(live) = &live {
                if live.live {
                    badge(ui, "EN VIVO", p.red, p.accent_weak);
                }
            }
        });
        info.add_space(2.0);
        if let Some(live) = &live {
            if live.live {
                if !live.title.is_empty() {
                    info.add(
                        egui::Label::new(
                            egui::RichText::new(&live.title).size(12.0).color(p.text),
                        )
                        .truncate(),
                    );
                }
                info.label(
                    egui::RichText::new(format!(
                        "{}  ·  {} espectadores",
                        if live.game.is_empty() {
                            "Sin categoría"
                        } else {
                            live.game.as_str()
                        },
                        format_count(live.viewers)
                    ))
                    .size(11.0)
                    .color(p.muted),
                );
            } else {
                info.label(
                    egui::RichText::new("Sin emisión")
                        .size(11.0)
                        .color(p.dim),
                );
            }
        } else {
            info.label(
                egui::RichText::new("Estado desconocido")
                    .size(11.0)
                    .color(p.dim),
            );
        }
        info.add_space(4.0);
        self.action_buttons(
            &mut info,
            &login,
            &display,
            RowKind::Followed,
            false,
            false,
            favorite,
            false,
            actions,
            &p,
        );

        if response.double_clicked() {
            actions.push(RowAction::Play(login.clone()));
        }
        ui.set_opacity(prev_opacity);
    }

    #[allow(clippy::too_many_arguments)]
    fn stream_card(
        &mut self,
        ui: &mut egui::Ui,
        login: &str,
        display: &str,
        game: &str,
        title: &str,
        viewers: u64,
        thumbnail_url: &str,
        is_live: bool,
        actions: &mut Vec<RowAction>,
    ) {
        let width = ui.available_width().max(140.0);
        let thumb_h = (width - 20.0) * 9.0 / 16.0;
        let height = thumb_h + 128.0;
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
        if !ui.is_rect_visible(rect) {
            return;
        }
        let p = self.palette();
        let key = login.to_lowercase();
        let live = LiveInfo {
            live: is_live,
            viewers,
            game: game.to_string(),
            title: title.to_string(),
            thumbnail_url: thumbnail_url.to_string(),
            user_id: String::new(),
        };
        let favorite = self.is_favorite(&key);

        let (enter_t, enter_y) = self.card_enter(ui, &key);
        let prev_opacity = ui.opacity();
        ui.set_opacity(prev_opacity * enter_t);
        let hover_t = ui.ctx().animate_bool_with_time(
            ui.make_persistent_id(("stream_card", key.as_str())),
            response.hovered(),
            0.15,
        );
        let lift = -3.0 * hover_t;
        let rect = rect.translate(egui::vec2(0.0, lift + enter_y));
        if hover_t > 0.01 {
            anim::shadow(
                ui.painter(),
                rect,
                14.0,
                [0, 6],
                16,
                egui::Color32::from_black_alpha((36.0 * hover_t) as u8),
            );
        }
        let fill = theme::lerp_color(p.surface, p.surface3, hover_t * 0.6);
        let border = theme::lerp_color(p.border, p.accent, hover_t * 0.8);
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(14), fill);
        ui.painter().rect_stroke(
            rect,
            egui::CornerRadius::same(14),
            egui::Stroke::new(1.0, border),
            egui::StrokeKind::Inside,
        );

        let inner = rect.shrink(10.0);
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        let thumb_width = child.available_width();
        self.thumbnail(
            &mut child,
            egui::vec2(thumb_width, thumb_h),
            &Some(live.clone()),
            &key,
            true,
        );
        child.add_space(6.0);
        child.horizontal(|ui| {
            self.avatar(ui, &key, 24.0);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(display)
                        .size(14.0)
                        .strong()
                        .color(p.text),
                )
                .truncate(),
            );
        });
        child.add_space(1.0);
        if is_live {
            child.add(
                egui::Label::new(
                    egui::RichText::new(if title.is_empty() {
                        "Sin título"
                    } else {
                        title
                    })
                    .size(11.5)
                    .color(p.muted),
                )
                .truncate(),
            );
            child.add(
                egui::Label::new(
                    egui::RichText::new(format!(
                        "{} · {} esp.",
                        if game.is_empty() {
                            "Sin categoría"
                        } else {
                            game
                        },
                        format_count(viewers)
                    ))
                    .size(10.5)
                    .color(p.dim),
                )
                .truncate(),
            );
        } else {
            child.label(
                egui::RichText::new("Sin emisión")
                    .size(11.0)
                    .color(p.dim),
            );
        }
        child.add_space(4.0);
        self.action_buttons(
            &mut child,
            &key,
            display,
            RowKind::Followed,
            false,
            false,
            favorite,
            false,
            actions,
            &p,
        );

        if response.double_clicked() {
            actions.push(RowAction::Play(key.clone()));
        }
        ui.set_opacity(prev_opacity);
    }

    fn game_card(&mut self, ui: &mut egui::Ui, game: &Game, selected: bool) -> bool {
        let width = ui.available_width().max(100.0);
        let cover_h = width * 4.0 / 3.0;
        let height = cover_h + 42.0;
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
        if !ui.is_rect_visible(rect) {
            return false;
        }
        let p = self.palette();
        let (enter_t, enter_y) = self.card_enter(ui, &game.id);
        let prev_opacity = ui.opacity();
        ui.set_opacity(prev_opacity * enter_t);
        let hover_t = ui.ctx().animate_bool_with_time(
            ui.make_persistent_id(("game_card", game.id.as_str())),
            response.hovered(),
            0.15,
        );
        let lift = -2.0 * hover_t;
        let rect = rect.translate(egui::vec2(0.0, lift + enter_y));
        if hover_t > 0.01 {
            anim::shadow(
                ui.painter(),
                rect,
                10.0,
                [0, 4],
                12,
                egui::Color32::from_black_alpha((30.0 * hover_t) as u8),
            );
        }
        let fill = theme::lerp_color(p.surface, p.surface3, hover_t * 0.7);
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(10), fill);
        let border = if selected {
            p.accent
        } else if response.hovered() {
            p.border2
        } else {
            p.border
        };
        ui.painter().rect_stroke(
            rect,
            egui::CornerRadius::same(10),
            egui::Stroke::new(if selected { 1.8 } else { 1.0 }, border),
            egui::StrokeKind::Inside,
        );

        let cover = egui::Rect::from_min_size(
            rect.min + egui::vec2(6.0, 6.0),
            egui::vec2(width - 12.0, cover_h - 12.0),
        );
        ui.painter()
            .rect_filled(cover, egui::CornerRadius::same(7), p.surface2);
        let mut painted = false;
        if !game.box_art_url.is_empty() {
            if let Some(texture) =
                self.images
                    .get(ui.ctx(), &game.box_art_url, 180, 240)
            {
                ui.painter().image(
                    texture.id(),
                    cover,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
                painted = true;
            }
        }
        if !painted {
            icons::paint(
                ui.painter(),
                egui::Rect::from_center_size(cover.center(), egui::vec2(34.0, 34.0)),
                Icon::Gamepad,
                p.dim,
                p.surface2,
            );
        }
        ui.painter().text(
            egui::pos2(rect.center().x, cover.max.y + 17.0),
            egui::Align2::CENTER_CENTER,
            ellipsize(&game.name, ((width - 10.0) / 6.4).max(6.0) as usize),
            egui::FontId::new(11.5, egui::FontFamily::Proportional),
            if selected { p.accent } else { p.text },
        );

        ui.set_opacity(prev_opacity);
        response.clicked()
    }

    fn logs_window(&mut self, ctx: &egui::Context) {
        if !self.logs_open {
            return;
        }
        let title = format!(
            "Registros — {}",
            self.logs_channel.as_deref().unwrap_or("sin canal")
        );
        let mut open = self.logs_open;
        egui::Window::new(title)
            .open(&mut open)
            .default_size([760.0, 440.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Actualizar").clicked() {
                        self.refresh_logs();
                    }
                    if ui.button("Copiar").clicked() {
                        ui.ctx().copy_text(self.logs_text.clone());
                    }
                    ui.weak(format!(
                        "Última actualización: hace {:.0} s",
                        self.logs_updated.elapsed().as_secs_f32()
                    ));
                });
                ui.separator();
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&self.logs_text).monospace().small(),
                            )
                            .wrap_mode(egui::TextWrapMode::Extend),
                        );
                    });
            });
        self.logs_open = open;
    }

    fn toast_overlay(&mut self, ctx: &egui::Context) {
        let Some((message, at)) = &self.toast else {
            return;
        };
        let elapsed = at.elapsed();
        const DURATION: f32 = 4.5;
        if elapsed.as_secs_f32() > DURATION {
            return;
        }
        let p = self.palette();
        let t = elapsed.as_secs_f32();
        // Entrada con rebote (como el toast de la web) y salida suave.
        let enter = anim::ease((t / 0.25).min(1.0), anim::Easing::BackOut);
        let exit = if t > DURATION - 0.35 {
            ((DURATION - t) / 0.35).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let alpha = (enter.min(exit)).clamp(0.0, 1.0);
        let offset_y = 26.0 * (1.0 - enter) - 10.0 * (1.0 - exit);

        egui::Area::new(egui::Id::unique("toast_overlay"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-18.0, -48.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let (icon, icon_color) = toast_icon(message);
                let font = egui::FontId::new(12.0, egui::FontFamily::Proportional);
                let text_color = theme::with_alpha(p.text, alpha);
                let galley = ui
                    .painter()
                    .layout_no_wrap(message.clone(), font, text_color);
                let icon_w = if icon.is_some() { 20.0 } else { 0.0 };
                let width = galley.size().x + icon_w + 34.0;
                let height = galley.size().y + 18.0;
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
                let rect = rect.translate(egui::vec2(0.0, offset_y));
                anim::shadow(
                    ui.painter(),
                    rect,
                    9.0,
                    [0, 6],
                    18,
                    egui::Color32::from_black_alpha((52.0 * alpha) as u8),
                );
                ui.painter().rect_filled(
                    rect,
                    egui::CornerRadius::same(9),
                    theme::with_alpha(p.surface3, alpha),
                );
                ui.painter().rect_stroke(
                    rect,
                    egui::CornerRadius::same(9),
                    egui::Stroke::new(1.0, theme::with_alpha(p.accent_mid, alpha)),
                    egui::StrokeKind::Inside,
                );
                if let Some(icon) = icon {
                    let icon_rect = egui::Rect::from_center_size(
                        egui::pos2(rect.min.x + 18.0, rect.center().y),
                        egui::vec2(15.0, 15.0),
                    );
                    icons::paint(
                        ui.painter(),
                        icon_rect,
                        icon,
                        theme::with_alpha(icon_color, alpha),
                        p.surface3,
                    );
                }
                ui.painter().galley(
                    egui::pos2(
                        rect.min.x + 17.0 + icon_w,
                        rect.center().y - galley.size().y / 2.0,
                    ),
                    galley,
                    text_color,
                );
                // Barra de drenaje del tiempo restante.
                let remaining = (1.0 - t / DURATION).clamp(0.0, 1.0);
                let bar = egui::Rect::from_min_size(
                    egui::pos2(rect.min.x, rect.max.y - 3.0),
                    egui::vec2(rect.width() * remaining, 3.0),
                );
                ui.painter().rect_filled(
                    bar,
                    egui::CornerRadius::same(2),
                    theme::with_alpha(p.accent, alpha),
                );
            });
        if t < 0.28 || t > DURATION - 0.38 {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    // -- chat y puntos de canal ---------------------------------------------

    /// Arranca el hilo del chat si no está en marcha.
    fn start_chat_thread(&mut self) {
        if self.chat_thread.is_some() {
            return;
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<chat::Command>();
        self.chat_tx = tx;
        let events = self.chat_events_tx.clone();
        let token = self.token.clone();
        let login = self.my_login.clone();
        self.chat_thread = Some(chat::spawn(token, login, events, rx));
    }

    /// Cierra el hilo del chat (libera la conexión IRC).
    fn stop_chat_thread(&mut self) {
        let _ = self.chat_tx.send(chat::Command::Shutdown);
        if let Some(handle) = self.chat_thread.take() {
            let _ = handle.join();
        }
    }

    /// Cierra el chat **por completo**: detiene la conexión y libera la
    /// memoria de mensajes, emotes y texturas.
    fn chat_close(&mut self) {
        self.stop_chat_thread();
        self.chat_messages.clear();
        self.chat_stats.clear();
        self.chat_emotes.clear();
        self.chat_channels.clear();
        self.chat_channel = None;
        self.my_emotes.clear();
        self.seven_global.clear();
        self.seven_mine.clear();
        self.seven_channel.clear();
        self.external_global.clear();
        self.external_channel.clear();
        self.external_sources.clear();
        self.emote_indexes.clear();
        self.emote_lists.clear();
        self.chat_cache_dir = None;
        self.chat_seg_cache.clear();
        self.chat_row_heights.clear();
        self.chat_unread.clear();
        self.chat_filter.clear();
        self.chat_filter_open = false;
        self.chat_settings_open = false;
        self.chat_history.clear();
        self.chat_history_pos = None;
        self.chat_completion = None;
        self.chat_history_requested.clear();
        self.chat_reply = None;
        self.chat_follow = true;
        self.chat_input.clear();
        self.images.purge("cdn.7tv.app");
        self.images.purge("betterttv.net");
        self.images.purge("frankerfacez.com");
        self.chat_panel = false;
        self.set_toast("Chat cerrado y memoria liberada");
    }

    // -- caché de emotes en disco -------------------------------------------

    /// Apartado de Ajustes para revisar y limpiar la caché por canal.
    fn emote_cache_section(&mut self, ui: &mut egui::Ui, p: &Palette) {
        if !self.emote_cache_scanned {
            self.rescan_emote_cache();
        }
        ui.weak(
            "Las listas y las imágenes de los emotes se guardan en emotes/<canal>. \
             Al volver al mismo directo se usan al instante y, pasados unos minutos, \
             se comprueba si el canal añadió o quitó emotes.",
        );
        ui.add_space(6.0);
        let total: u64 = self
            .emote_cache_entries
            .iter()
            .map(|entry| entry.bytes)
            .sum();
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "{} carpetas · {}",
                    self.emote_cache_entries.len(),
                    emote_cache::human_bytes(total)
                ))
                .size(11.5)
                .color(p.text),
            );
            if ui.small_button("Actualizar lista").clicked() {
                self.rescan_emote_cache();
            }
            if ui.small_button("Abrir carpeta").clicked() {
                open_path(&emote_cache::root());
            }
        });
        ui.add_space(5.0);
        if self.emote_cache_entries.is_empty() {
            ui.label(
                egui::RichText::new("Todavía no hay emotes guardados.")
                    .size(11.0)
                    .color(p.dim),
            );
            return;
        }
        if self.emote_cache_selected.is_empty() {
            self.emote_cache_selected = self.emote_cache_entries[0].channel.clone();
        }
        let current = self.emote_cache_selected.clone();
        let mut selected_label = emote_cache::label(&current);
        if let Some(entry) = self
            .emote_cache_entries
            .iter()
            .find(|entry| entry.channel == current)
        {
            selected_label = format!(
                "{} · {} · {} archivos",
                emote_cache::label(&entry.channel),
                emote_cache::human_bytes(entry.bytes),
                entry.files
            );
        }
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("emote_cache_channel")
                .width(280.0)
                .selected_text(selected_label)
                .show_ui(ui, |ui| {
                    for entry in &self.emote_cache_entries {
                        let text = format!(
                            "{} · {}",
                            emote_cache::label(&entry.channel),
                            emote_cache::human_bytes(entry.bytes)
                        );
                        ui.selectable_value(
                            &mut self.emote_cache_selected,
                            entry.channel.clone(),
                            text,
                        );
                    }
                });
            if ui.button("Limpiar canal").clicked() {
                let freed = emote_cache::clear_channel(&current);
                // Los emotes que ya están en memoria se dejan intactos: el
                // chat sigue funcionando y la carpeta se recreará la próxima
                // vez que se abra ese canal.
                self.rescan_emote_cache();
                self.set_toast(format!(
                    "Caché de {} borrada ({})",
                    emote_cache::label(&current),
                    emote_cache::human_bytes(freed)
                ));
            }
        });
        if let Some(entry) = self
            .emote_cache_entries
            .iter()
            .find(|entry| entry.channel == current)
        {
            let age = entry
                .updated
                .map(format_age)
                .unwrap_or_else(|| "sin fecha".to_string());
            ui.add_space(3.0);
            ui.label(
                egui::RichText::new(format!(
                    "Ocupa {} en {} archivos · actualizada {}",
                    emote_cache::human_bytes(entry.bytes),
                    entry.files,
                    age
                ))
                .size(10.5)
                .color(p.muted),
            );
        }
        ui.add_space(6.0);
        if ui
            .button(egui::RichText::new("Limpiar toda la caché").color(p.red))
            .clicked()
        {
            let freed = emote_cache::clear_all();
            self.emote_cache_selected.clear();
            self.rescan_emote_cache();
            self.set_toast(format!(
                "Toda la caché de emotes borrada ({})",
                emote_cache::human_bytes(freed)
            ));
        }
    }

    /// Vuelve a medir las carpetas de la caché de emotes.
    fn rescan_emote_cache(&mut self) {
        self.emote_cache_entries = emote_cache::scan();
        self.emote_cache_scanned = true;
        if !self.emote_cache_entries.is_empty()
            && !self
                .emote_cache_entries
                .iter()
                .any(|entry| entry.channel == self.emote_cache_selected)
        {
            self.emote_cache_selected = self.emote_cache_entries[0].channel.clone();
        }
    }

    /// Abre el chat web de Twitch en el navegador del sistema.
    fn chat_open_web(&mut self) {
        let Some(channel) = self.chat_channel.clone() else {
            return;
        };
        let url = format!("https://www.twitch.tv/popout/{channel}/chat?popout=");
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", &url])
            .spawn();
    }

    /// Abre Chatterino con el canal actual (si está instalado).
    fn chat_open_chatterino(&mut self) {
        let Some(channel) = self.chat_channel.clone() else {
            return;
        };
        let Some(path) = self.chatterino_path() else {
            self.set_toast("Chatterino no encontrado: indícalo en Ajustes");
            return;
        };
        let _ = std::process::Command::new(&path)
            .args(["--channels", &format!("twitch:{channel}")])
            .spawn();
    }

    /// Ruta del ejecutable de Chatterino.
    fn chatterino_path(&self) -> Option<std::path::PathBuf> {
        let configured = self.cfg.chatterino.trim();
        if !configured.is_empty() {
            let path = std::path::PathBuf::from(configured);
            if path.exists() {
                return Some(path);
            }
        }
        for candidate in [
            r"C:\Program Files\Chatterino2\Chatterino2.exe",
            r"C:\Program Files (x86)\Chatterino2\Chatterino2.exe",
        ] {
            let path = std::path::PathBuf::from(candidate);
            if path.exists() {
                return Some(path);
            }
        }
        None
    }

    fn chat_join(&mut self, channel: &str) {
        let channel = channel.trim().to_lowercase();
        if channel.is_empty() {
            return;
        }
        self.start_chat_thread();
        if !self.chat_channels.contains(&channel) {
            self.chat_channels.push(channel.clone());
        }
        if self.chat_channel.is_none() {
            self.chat_channel = Some(channel.clone());
        }
        self.chat_unread.remove(&channel);
        let _ = self.chat_tx.send(chat::Command::Join(channel.clone()));
        self.load_seven_channel(&channel);
        self.load_external_emotes(&channel);
        self.load_external_global();
        self.start_sevent(&channel);
        self.request_history(&channel);
        self.mining_refresh_channel(&channel);
    }

    /// Pide el historial reciente del canal (como Chatterino al unirse).
    fn request_history(&mut self, channel: &str) {
        let channel = channel.trim().to_lowercase();
        if channel.is_empty() || self.chat_history_requested.contains(&channel) {
            return;
        }
        self.chat_history_requested.insert(channel.clone());
        let events = self.tx.clone();
        let target = channel.clone();
        std::thread::Builder::new()
            .name("chat-history".into())
            .spawn(move || {
                let result = chat::recent_messages(&target);
                let _ = events.send(Event::History(target, result));
            })
            .ok();
    }

    /// Inserta el historial al principio del canal, sin duplicar mensajes.
    fn merge_history(&mut self, _channel: &str, events: Vec<chat::ChatEvent>) {
        if events.is_empty() {
            return;
        }
        let fresh: Vec<chat::ChatEvent> = events
            .into_iter()
            .filter(|event| {
                !event.id.is_empty()
                    && !self
                        .chat_messages
                        .iter()
                        .any(|existing| existing.id == event.id)
            })
            .collect();
        if fresh.is_empty() {
            return;
        }
        let mut all = std::mem::take(&mut self.chat_messages);
        all.extend(fresh);
        all.sort_by_key(|event| event.at);
        all.dedup_by(|a, b| !a.id.is_empty() && a.id == b.id);
        self.chat_messages = all;
        const MAX_MESSAGES: usize = 400;
        if self.chat_messages.len() > MAX_MESSAGES {
            let excess = self.chat_messages.len() - MAX_MESSAGES;
            self.chat_messages.drain(0..excess);
        }
    }

    fn chat_leave(&mut self, channel: &str) {
        let channel = channel.trim().to_lowercase();
        self.chat_channels.retain(|name| name != &channel);
        self.chat_messages.retain(|event| event.channel != channel);
        self.chat_unread.remove(&channel);
        self.chat_history_requested.remove(&channel);
        if self.chat_channel.as_deref() == Some(channel.as_str()) {
            self.chat_channel = self.chat_channels.first().cloned();
        }
        if self
            .chat_reply
            .as_ref()
            .is_some_and(|(reply_channel, _, _)| reply_channel == &channel)
        {
            self.chat_reply = None;
        }
        let _ = self.chat_tx.send(chat::Command::Part(channel));
    }

    fn chat_send(&mut self) {
        let text = self.chat_input.trim().to_string();
        if std::env::var("SH_DEBUG").is_ok() {
            eprintln!(
                "[app] chat_send: texto={:?} canal={:?} token={}",
                text,
                self.chat_channel,
                !self.token.is_empty()
            );
        }
        if text.is_empty() {
            return;
        }
        let original = text.clone();
        self.chat_completion = None;
        self.chat_history_pos = None;
        // Comandos que resuelve la propia interfaz.
        let lower = text.to_lowercase();
        if lower == "/help" || lower == "/ayuda" {
            self.set_toast(
                "Comandos: /me, /w <usuario> <texto>, /clear, /ping, /uptime, /history",
            );
            self.chat_input.clear();
            return;
        }
        if lower == "/clear" || lower == "/limpiar" {
            if let Some(channel) = self.chat_channel.clone() {
                self.chat_messages.retain(|event| event.channel != channel);
            }
            self.chat_input.clear();
            return;
        }
        if lower == "/history" || lower == "/historial" {
            if let Some(channel) = self.chat_channel.clone() {
                self.chat_history_requested.remove(&channel);
                self.request_history(&channel);
                self.set_toast("Cargando historial reciente...");
            }
            self.chat_input.clear();
            return;
        }
        // Whispers: /w usuario texto
        if let Some(rest) = text.strip_prefix("/w ").or_else(|| text.strip_prefix("/whisper ")) {
            let mut parts = rest.splitn(2, ' ');
            let user = parts.next().unwrap_or_default().trim().to_lowercase();
            let body = parts.next().unwrap_or_default().trim();
            if user.is_empty() || body.is_empty() {
                self.set_toast("Uso: /w <usuario> <mensaje>");
                return;
            }
            if self
                .chat_tx
                .send(chat::Command::Whisper {
                    user: user.clone(),
                    text: body.to_string(),
                })
                .is_err()
            {
                self.set_toast("El hilo del chat no responde");
            }
            self.chat_messages.push(chat::ChatEvent {
                kind: chat::Kind::Whisper,
                channel: String::new(),
                login: self.my_login.clone(),
                display: self.display_for_self(),
                color: None,
                badges: Vec::new(),
                text: format!("→ {user}: {body}"),
                emotes: Vec::new(),
                bits: 0,
                at: std::time::SystemTime::now(),
                connected: None,
                emote_sets: Vec::new(),
                id: String::new(),
                reply: None,
            });
            self.chat_input.clear();
            return;
        }
        let Some(channel) = self.chat_channel.clone() else {
            return;
        };
        if self.token.is_empty() {
            self.set_toast("Necesitas un token con chat:edit para escribir");
            return;
        }
        let reply = self.chat_reply.clone();
        let reply_to = reply.as_ref().map(|(_, id, _)| id.clone());
        if self
            .chat_tx
            .send(chat::Command::Send {
                channel: channel.clone(),
                text: text.clone(),
                reply_to,
            })
            .is_err()
        {
            self.set_toast("El hilo del chat no responde");
        }
        // Twitch no devuelve por IRC los mensajes que envías tú, así que se
        // muestran al instante en local (como en cualquier cliente).
        let display = self.display_for_self();
        // Un "/me texto" se envía como acción.
        let (kind, text) = match text.strip_prefix("/me ") {
            Some(action) => (chat::Kind::Action, action.to_string()),
            None => (chat::Kind::Message, text),
        };
        self.chat_messages.push(chat::ChatEvent {
            kind,
            channel,
            login: self.my_login.clone(),
            display,
            color: None,
            badges: Vec::new(),
            text,
            emotes: Vec::new(),
            bits: 0,
            at: std::time::SystemTime::now(),
            connected: None,
            emote_sets: Vec::new(),
            id: String::new(),
            reply: reply.map(|(_, _, display)| (display.to_lowercase(), String::new())),
        });
        self.chat_reply = None;
        self.chat_input.clear();
        self.chat_history.retain(|entry| entry != &original);
        self.chat_history.push(original);
        if self.chat_history.len() > 50 {
            self.chat_history.remove(0);
        }
        self.chat_history_pos = None;
        self.chat_follow = true;
    }

    fn display_for_self(&self) -> String {
        self.followed_user
            .as_ref()
            .map(|user| user.display_name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.my_login.clone())
    }

    /// Aplica la identidad (token/login) al hilo del chat.
    fn chat_identity(&mut self) {
        let login = if self.my_login.is_empty() {
            self.followed_user
                .as_ref()
                .map(|user| user.login.clone())
                .unwrap_or_default()
        } else {
            self.my_login.clone()
        };
        let _ = self.chat_tx.send(chat::Command::Identity {
            token: self.token.clone(),
            login,
        });
    }

    /// Descarga la lista de emotes del usuario (sus emotes de suscripción).
    ///
    /// Los conjuntos numéricos se piden por GQL (con rescate por Helix si
    /// fallan) y los UUID que llegan por `GLOBALUSERSTATE` —conjuntos nuevos
    /// de Twitch— se piden directamente a Helix.
    fn fetch_my_emotes(&mut self, sets: Vec<String>) {
        if sets.is_empty() || self.token.is_empty() {
            return;
        }
        let token = self.token.clone();
        let client_id = self.credentials().map(|(id, _)| id).unwrap_or_default();
        let events = self.tx.clone();
        std::thread::Builder::new()
            .name("emotes".into())
            .spawn(move || {
                let debug = std::env::var("SH_DEBUG").is_ok();
                let mut list: Vec<crate::gql::EmoteSet> = Vec::new();
                let mut helix_todo: Vec<String> = Vec::new();

                // -- GQL: conjuntos numéricos clásicos -----------------------
                let numeric: Vec<String> = sets
                    .iter()
                    .filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
                    .cloned()
                    .collect();
                if !numeric.is_empty() {
                    if let Ok(client) = crate::gql::GqlClient::new(&token) {
                        let (mut twitch_list, failed) = client.emote_sets(&numeric);
                        list.append(&mut twitch_list);
                        for set_id in failed {
                            if !helix_todo.contains(&set_id) {
                                helix_todo.push(set_id);
                            }
                        }
                    }
                }

                // Los conjuntos con UUID (nuevos de Twitch) no existen en GQL:
                // se rescatan directamente por Helix.
                for set_id in &sets {
                    if !set_id.is_empty()
                        && !set_id.chars().all(|c| c.is_ascii_digit())
                        && !helix_todo.contains(set_id)
                    {
                        helix_todo.push(set_id.clone());
                    }
                }

                if !helix_todo.is_empty() {
                    if debug {
                        eprintln!(
                            "[chat] {} conjuntos se rescatan por Helix",
                            helix_todo.len()
                        );
                    }
                    if let Ok(helix) = TwitchClient::new(&client_id, &token) {
                        type RescuedSet = (String, String, Vec<(String, String)>);
                        let mut rescued: Vec<RescuedSet> = Vec::new();
                        let mut owner_ids: Vec<String> = Vec::new();
                        for set_id in &helix_todo {
                            let Ok(emotes) = helix.get_emote_set(set_id) else {
                                continue;
                            };
                            if emotes.is_empty() {
                                continue;
                            }
                            let owner_id = emotes[0].owner_id.clone();
                            for emote in &emotes {
                                if !emote.owner_id.is_empty()
                                    && !owner_ids.contains(&emote.owner_id)
                                {
                                    owner_ids.push(emote.owner_id.clone());
                                }
                            }
                            rescued.push((
                                set_id.clone(),
                                owner_id,
                                emotes
                                    .into_iter()
                                    .map(|emote| (emote.id, emote.name))
                                    .collect(),
                            ));
                        }
                        let mut logins: HashMap<String, String> = HashMap::new();
                        if !owner_ids.is_empty() {
                            if let Ok(users) = helix.get_users_by_ids(&owner_ids) {
                                for (_, user) in users {
                                    logins.insert(user.id, user.login.to_lowercase());
                                }
                            }
                        }
                        for (set_id, owner_id, emotes) in rescued {
                            let owner = logins.get(&owner_id).cloned().unwrap_or_default();
                            if debug {
                                eprintln!(
                                    "[chat] conjunto {set_id}: {} emotes vía Helix (#{owner})",
                                    emotes.len()
                                );
                            }
                            list.push(crate::gql::EmoteSet {
                                id: set_id,
                                owner,
                                emotes,
                            });
                        }
                    }
                }

                let count = list.len();
                let mut all: Vec<(String, String, String)> = Vec::new();
                let mut seen: HashSet<(String, String)> = HashSet::new();
                for set in list {
                    for (id, name) in set.emotes {
                        if seen.insert((id.clone(), set.owner.clone())) {
                            all.push((id, name, set.owner.clone()));
                        }
                    }
                }
                if debug {
                    eprintln!(
                        "[chat] {} conjuntos, {} emotes del usuario",
                        count,
                        all.len()
                    );
                }
                if !all.is_empty() {
                    let _ = events.send(Event::Emotes(all));
                }
            })
            .ok();
    }

    /// Carga los emotes globales de 7TV (caché primero; luego comprueba cambios).
    fn load_seven_global(&mut self) {
        if !self.seven_global.is_empty() {
            return;
        }
        let events = self.tx.clone();
        std::thread::Builder::new()
            .name("seven-tv".into())
            .spawn(move || {
                let cached = emote_cache::read_list::<seven_tv::SevenEmote>(
                    emote_cache::GLOBAL,
                    "7tv",
                );
                let mut stale = true;
                if let Some((at, items)) = &cached {
                    stale = emote_cache::is_stale(*at);
                    let _ = events.send(Event::SevenGlobal(items.clone()));
                }
                if !stale {
                    return;
                }
                if let Ok(emotes) = seven_tv::global_emotes() {
                    let changed = cached
                        .as_ref()
                        .map(|(_, old)| !seven_tv::same_emotes(old, &emotes))
                        .unwrap_or(true);
                    emote_cache::write_list(emote_cache::GLOBAL, "7tv", &emotes);
                    if changed {
                        let _ = events.send(Event::SevenGlobal(emotes));
                    }
                }
            })
            .ok();
    }

    /// Carga los emotes de 7TV del usuario (caché primero).
    fn load_seven_mine(&mut self) {
        if self.my_id.is_empty() || !self.seven_mine.is_empty() {
            return;
        }
        let id = self.my_id.clone();
        let events = self.tx.clone();
        std::thread::Builder::new()
            .name("seven-tv".into())
            .spawn(move || {
                let cached =
                    emote_cache::read_list::<seven_tv::SevenEmote>(emote_cache::MINE, "7tv");
                let mut stale = true;
                if let Some((at, items)) = &cached {
                    stale = emote_cache::is_stale(*at);
                    let _ = events.send(Event::SevenMine(items.clone()));
                }
                if !stale {
                    return;
                }
                if let Ok(emotes) = seven_tv::user_emotes(&id) {
                    let changed = cached
                        .as_ref()
                        .map(|(_, old)| !seven_tv::same_emotes(old, &emotes))
                        .unwrap_or(true);
                    emote_cache::write_list(emote_cache::MINE, "7tv", &emotes);
                    if changed {
                        let _ = events.send(Event::SevenMine(emotes));
                    }
                }
            })
            .ok();
    }

    /// Carga los emotes de 7TV de un canal (caché primero; luego cambios).
    fn load_seven_channel(&mut self, channel: &str) {
        let channel = channel.to_lowercase();
        if self.seven_channel.contains_key(&channel) {
            return;
        }
        // Se resuelve por Helix cuando el canal está offline: `live` puede no
        // tenerlo y sus emotes de 7TV deben cargarse igualmente.
        let Some(id) = self.channel_id(&channel) else {
            return;
        };
        let events = self.tx.clone();
        std::thread::Builder::new()
            .name("seven-tv".into())
            .spawn(move || {
                let cached = emote_cache::read_list::<seven_tv::SevenEmote>(&channel, "7tv");
                let mut stale = true;
                if let Some((at, items)) = &cached {
                    stale = emote_cache::is_stale(*at);
                    let _ = events.send(Event::SevenChannel(channel.clone(), items.clone()));
                }
                if !stale {
                    return;
                }
                if let Ok(emotes) = seven_tv::user_emotes(&id) {
                    let changed = cached
                        .as_ref()
                        .map(|(_, old)| !seven_tv::same_emotes(old, &emotes))
                        .unwrap_or(true);
                    // Se actualiza la carpeta del canal aunque no cambie nada
                    // (marca de última comprobación) y solo se avisa a la UI
                    // si la lista es distinta.
                    emote_cache::write_list(&channel, "7tv", &emotes);
                    if changed {
                        let _ = events.send(Event::SevenChannel(channel, emotes));
                    }
                }
            })
            .ok();
    }

    /// Emotes disponibles para un canal, en orden determinista y sin mezclar
    /// los de otros canales. Se cachea por canal para no recalcularlo cada
    /// frame; el índice nombre → URL se construye a la vez.
    fn emotes_for_channel(
        &mut self,
        channel: &str,
    ) -> Arc<Vec<EmoteEntry>> {
        let channel = channel.to_lowercase();
        if self.emote_lists_dirty {
            self.emote_lists.clear();
            self.emote_indexes.clear();
            self.emote_lists_dirty = false;
            self.emote_generation = self.emote_generation.wrapping_add(1);
        }
        if let Some(list) = self.emote_lists.get(&channel) {
            // Clonar el `Arc` es solo incrementar un contador.
            return Arc::clone(list);
        }
        let mut out: Vec<EmoteEntry> = Vec::new();
        // Las claves se guardan en minúsculas para que la deduplicación no
        // dependa de mayúsculas y coincida con la búsqueda del chat.
        let mut seen: HashSet<String> = HashSet::new();
        let push_seven =
            |emotes: &[seven_tv::SevenEmote],
             owner: &str,
             out: &mut Vec<EmoteEntry>,
             seen: &mut HashSet<String>| {
                for emote in emotes {
                    if seen.insert(emote.name.to_lowercase()) {
                        out.push(EmoteEntry {
                            name: emote.name.clone(),
                            url: emote.url.clone(),
                            origin: EmoteOrigin::SevenTv,
                            owner: owner.to_string(),
                        });
                    }
                }
            };
        // 1. 7TV: primero los del canal, luego los personales y los globales.
        if let Some(channel_emotes) = self.seven_channel.get(&channel) {
            push_seven(channel_emotes, &channel, &mut out, &mut seen);
        }
        let my_login = self.my_login.to_lowercase();
        let mine_owner = if my_login.is_empty() {
            channel.clone()
        } else {
            my_login
        };
        push_seven(&self.seven_mine, &mine_owner, &mut out, &mut seen);
        push_seven(&self.seven_global, "", &mut out, &mut seen);
        // 2. Twitch: los del canal actual y los globales, y después TODOS los
        //    de otros canales (emotes de suscripción del usuario), agrupados
        //    por canal para el selector.
        let twitch_url = |id: &str| {
            format!("https://static-cdn.jtvnw.net/emoticons/v2/{id}/default/dark/2.0")
        };
        for (id, name, owner) in &self.my_emotes {
            if (owner != &channel && !owner.is_empty())
                || !seen.insert(name.to_lowercase())
            {
                continue;
            }
            out.push(EmoteEntry {
                name: name.clone(),
                url: twitch_url(id),
                origin: EmoteOrigin::Twitch,
                owner: owner.clone(),
            });
        }
        let mut others: Vec<&(String, String, String)> = self
            .my_emotes
            .iter()
            .filter(|(_, _, owner)| !owner.is_empty() && owner != &channel)
            .collect();
        others.sort_by(|a, b| a.2.cmp(&b.2));
        for (id, name, owner) in others {
            if !seen.insert(name.to_lowercase()) {
                continue;
            }
            out.push(EmoteEntry {
                name: name.clone(),
                url: twitch_url(id),
                origin: EmoteOrigin::Twitch,
                owner: owner.to_string(),
            });
        }
        // 3. BetterTTV y FrankerFaceZ del canal y globales. La lista viene
        //    mezclada, así que el origen se deduce del dominio de la imagen.
        let origin_of = |url: &str| {
            if url.contains("betterttv.net") {
                EmoteOrigin::Bttv
            } else {
                EmoteOrigin::Ffz
            }
        };
        if let Some(list) = self.external_channel.get(&channel) {
            for (name, url) in list {
                if seen.insert(name.to_lowercase()) {
                    out.push(EmoteEntry {
                        name: name.clone(),
                        url: url.clone(),
                        origin: origin_of(url),
                        owner: channel.clone(),
                    });
                }
            }
        }
        for (name, url) in &self.external_global {
            if seen.insert(name.to_lowercase()) {
                out.push(EmoteEntry {
                    name: name.clone(),
                    url: url.clone(),
                    origin: origin_of(url),
                    owner: String::new(),
                });
            }
        }
        // Se cachea por canal: con 1500 emotes recalcularlo cada frame era
        // carísimo (comparaciones O(n²) y miles de asignaciones).
        let mut index: HashMap<String, String> = HashMap::with_capacity(out.len());
        for entry in &out {
            index
                .entry(entry.name.to_lowercase())
                .or_insert_with(|| entry.url.clone());
        }
        let list = Arc::new(out);
        if std::env::var("SH_DEBUG").is_ok() {
            let mut seven = 0;
            let mut twitch = 0;
            let mut external = 0;
            for entry in list.iter() {
                match entry.origin {
                    EmoteOrigin::SevenTv => seven += 1,
                    EmoteOrigin::Twitch => twitch += 1,
                    EmoteOrigin::Bttv | EmoteOrigin::Ffz => external += 1,
                }
            }
            let mut subs = 0;
            for (_, name, owner) in &self.my_emotes {
                if owner == &channel && seen.contains(&name.to_lowercase()) {
                    subs += 1;
                }
            }
            eprintln!(
                "[emotes] #{channel}: {} total (7TV {seven}, Twitch {twitch} de los que {subs} son del canal, BTTV/FFZ {external})",
                list.len()
            );
        }
        self.emote_lists.insert(channel.clone(), Arc::clone(&list));
        self.emote_indexes.insert(channel, Arc::new(index));
        list
    }

    /// Índice nombre → URL del canal actual, para pintar emotes en el chat.
    /// Se recalcula como máximo una vez por cambio de listas.
    fn emote_index_for(&mut self, channel: &str) -> Arc<HashMap<String, String>> {
        self.emotes_for_channel(channel);
        self.emote_indexes
            .get(&channel.to_lowercase())
            .cloned()
            .unwrap_or_default()
    }

    /// Carpeta de caché del canal, memorizada para no resolverla en cada frame.
    fn channel_cache_dir(&mut self, channel: &str) -> std::path::PathBuf {
        if let Some((cached, dir)) = &self.chat_cache_dir {
            if cached == channel {
                return dir.clone();
            }
        }
        let dir = emote_cache::channel_dir(channel);
        self.chat_cache_dir = Some((channel.to_string(), dir.clone()));
        dir
    }

    /// ID numérico de un canal: lo consulta a Helix si aún no lo tenemos.
    fn channel_id(&mut self, channel: &str) -> Option<String> {
        let channel = channel.to_lowercase();
        if let Some(id) = self.channel_ids.get(&channel) {
            return Some(id.clone());
        }
        if let Some(info) = self.live.get(&channel) {
            if !info.user_id.is_empty() {
                let id = info.user_id.clone();
                self.channel_ids.insert(channel, id.clone());
                return Some(id);
            }
        }
        let (client_id, token) = self.credentials()?;
        let events = self.tx.clone();
        let target = channel.clone();
        std::thread::Builder::new()
            .name("helix-id".into())
            .spawn(move || {
                let Ok(client) = TwitchClient::new(&client_id, &token) else {
                    return;
                };
                if let Ok(users) = client.get_users(std::slice::from_ref(&target)) {
                    if let Some((login, user)) = users.into_iter().next() {
                        let _ = events.send(Event::ChannelId(target, user.id.clone()));
                        if !user.profile_image_url.is_empty() {
                            let _ = events.send(Event::UserAvatars(vec![(
                                login.to_lowercase(),
                                user.profile_image_url,
                            )]));
                        }
                    }
                }
            })
            .ok();
        None
    }

    /// Pide a Helix la imagen de perfil de los canales dueños de emotes para
    /// mostrarla en las cabeceras del selector.
    fn fetch_avatars(&mut self, logins: &[String]) {
        let mut pending: Vec<String> = Vec::new();
        for login in logins {
            let login = login.to_lowercase();
            if login.is_empty()
                || self.user_avatars.contains_key(&login)
                || !self.avatars_pending.insert(login.clone())
            {
                continue;
            }
            pending.push(login);
        }
        if pending.is_empty() {
            return;
        }
        let Some((client_id, token)) = self.credentials() else {
            // Sin credenciales no hay Helix: permitir reintentos más tarde.
            for login in &pending {
                self.avatars_pending.remove(login);
            }
            return;
        };
        let events = self.tx.clone();
        std::thread::Builder::new()
            .name("avatars".into())
            .spawn(move || {
                let Ok(client) = TwitchClient::new(&client_id, &token) else {
                    return;
                };
                if let Ok(users) = client.get_users(&pending) {
                    let list: Vec<(String, String)> = users
                        .into_iter()
                        .filter(|(_, user)| !user.profile_image_url.is_empty())
                        .map(|(login, user)| (login, user.profile_image_url))
                        .collect();
                    if !list.is_empty() {
                        let _ = events.send(Event::UserAvatars(list));
                    }
                }
            })
            .ok();
    }

    /// Carga los emotes de BetterTTV y FrankerFaceZ de un canal (con caché).
    fn load_external_emotes(&mut self, channel: &str) {
        let channel = channel.to_lowercase();
        if self.external_channel.contains_key(&channel) {
            return;
        }
        let Some(id) = self.channel_id(&channel) else {
            return;
        };
        let events = self.tx.clone();
        for (source, fetcher) in [
            (
                "bttv",
                emotes::bttv_channel as fn(&str) -> Result<Vec<emotes::ExternalEmote>, String>,
            ),
            (
                "ffz",
                emotes::ffz_channel as fn(&str) -> Result<Vec<emotes::ExternalEmote>, String>,
            ),
        ] {
            let id = id.clone();
            let channel = channel.clone();
            let events = events.clone();
            std::thread::Builder::new()
                .name("emotes".into())
                .spawn(move || {
                    let cached =
                        emote_cache::read_list::<emotes::ExternalEmote>(&channel, source);
                    let mut stale = true;
                    if let Some((at, items)) = &cached {
                        stale = emote_cache::is_stale(*at);
                        let pairs = to_pairs(items);
                        let _ = events.send(Event::ExternalEmotes(
                            channel.clone(),
                            source.into(),
                            pairs,
                        ));
                    }
                    if !stale {
                        return;
                    }
                    if let Ok(list) = fetcher(&id) {
                        let changed = cached
                            .as_ref()
                            .map(|(_, old)| !emotes::same_list(old, &list))
                            .unwrap_or(true);
                        emote_cache::write_list(&channel, source, &list);
                        if changed {
                            let pairs = to_pairs(&list);
                            let _ = events.send(Event::ExternalEmotes(
                                channel,
                                source.into(),
                                pairs,
                            ));
                        }
                    }
                })
                .ok();
        }
    }

    /// Carga los emotes globales de BetterTTV y FrankerFaceZ (con caché).
    fn load_external_global(&mut self) {
        if !self.external_global.is_empty() {
            return;
        }
        let events = self.tx.clone();
        for (source, fetcher) in [
            ("bttv", emotes::bttv_global as fn() -> Result<Vec<emotes::ExternalEmote>, String>),
            ("ffz", emotes::ffz_global as fn() -> Result<Vec<emotes::ExternalEmote>, String>),
        ] {
            let events = events.clone();
            std::thread::Builder::new()
                .name("emotes".into())
                .spawn(move || {
                    let cached = emote_cache::read_list::<emotes::ExternalEmote>(
                        emote_cache::GLOBAL,
                        source,
                    );
                    let mut stale = true;
                    if let Some((at, items)) = &cached {
                        stale = emote_cache::is_stale(*at);
                        let pairs = to_pairs(items);
                        let _ =
                            events.send(Event::ExternalGlobal(source.into(), pairs));
                    }
                    if !stale {
                        return;
                    }
                    if let Ok(list) = fetcher() {
                        let changed = cached
                            .as_ref()
                            .map(|(_, old)| !emotes::same_list(old, &list))
                            .unwrap_or(true);
                        emote_cache::write_list(emote_cache::GLOBAL, source, &list);
                        if changed {
                            let pairs = to_pairs(&list);
                            let _ =
                                events.send(Event::ExternalGlobal(source.into(), pairs));
                        }
                    }
                })
                .ok();
        }
    }

    /// Arranca los eventos en vivo de 7TV para un canal.
    fn start_sevent(&mut self, channel: &str) {
        let channel = channel.to_lowercase();
        if self.sevent_channels.contains(&channel) {
            return;
        }
        let Some(id) = self.channel_id(&channel) else {
            return;
        };
        self.sevent_channels.push(channel.clone());
        let events = self.sevent_events_tx.clone();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.sevent_stops.insert(channel, Arc::clone(&stop));
        let _ = sevent::spawn(id, events, stop);
    }

    fn handle_chat_event(&mut self, event: chat::ChatEvent) {
        if let Some(connected) = event.connected {
            self.chat_connected = connected;
            return;
        }
        if event.kind == chat::Kind::EmoteSets {
            if std::env::var("SH_DEBUG").is_ok() {
                eprintln!("[chat] emote-sets recibidos: {:?}", event.emote_sets);
            }
            self.chat_emote_sets = event.emote_sets.clone();
            self.fetch_my_emotes(event.emote_sets);
            return;
        }
        // Borrado de un mensaje concreto: se quita del historial.
        if event.kind == chat::Kind::Delete {
            if !event.id.is_empty() {
                self.chat_messages.retain(|item| item.id != event.id);
            }
            return;
        }
        // Limpieza completa del canal.
        if event.kind == chat::Kind::Clear {
            self.chat_messages.retain(|item| item.channel != event.channel);
            return;
        }
        let channel = event.channel.clone();
        let stats = self.chat_stats.entry(channel.clone()).or_default();
        if event.bits > 0 {
            stats.bits += event.bits;
        }
        if event.kind == chat::Kind::Message || event.kind == chat::Kind::Action {
            stats.messages += 1;
        }
        if event.kind == chat::Kind::System {
            let text = event.text.to_lowercase();
            if text.contains("raid") {
                stats.raids += 1;
            } else if text.contains("regaló") {
                stats.gifts += 1;
            } else {
                stats.subs += 1;
            }
        }
        // Emotes vistos: sirven para el selector de emotes.
        for emote in &event.emotes {
            if emote.end <= event.text.len() {
                self.chat_emotes
                    .entry(emote.id.clone())
                    .or_insert_with(|| event.text[emote.start..emote.end].to_string());
            }
        }
        // Contador de no leídos: solo si no es el canal abierto o el usuario
        // está mirando hacia arriba.
        let active = self.chat_channel.as_deref() == Some(channel.as_str());
        if !active || !self.chat_follow {
            *self.chat_unread.entry(channel).or_insert(0) += 1;
        }
        self.chat_messages.push(event);
        // Buffer acotado para no crecer sin límite.
        const MAX_MESSAGES: usize = 400;
        if self.chat_messages.len() > MAX_MESSAGES {
            let excess = self.chat_messages.len() - MAX_MESSAGES;
            self.chat_messages.drain(0..excess);
        }
    }

    /// Starts the background channel points service automatically (no manual steps).
    fn ensure_mining_started(&mut self) {
        if self.mining_running || !self.has_creds() {
            return;
        }
        let Some((client_id, token)) = self.credentials() else {
            return;
        };
        self.stop_mining_internal();
        let (mining_tx, commands) = mpsc::channel();
        self.mining_tx = mining_tx;
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.mining_stop = Some(Arc::clone(&stop));
        let events = self.mining_events_tx.clone();
        self.mining_running = true;
        self.mining_log
            .push("Puntos de canal activos automáticamente".into());

        // Re-register any currently playing channels so heartbeats start right away.
        if let Some(channel) = self.embedded_channel.clone() {
            let _ = self.mining_tx.send(mining::Command::Watch(channel));
        }
        for channel in self.running.keys().cloned().collect::<Vec<_>>() {
            let _ = self.mining_tx.send(mining::Command::Watch(channel));
        }
        if let Some(channel) = self.chat_channel.clone() {
            let _ = self
                .mining_tx
                .send(mining::Command::RefreshChannel(channel));
        }

        std::thread::Builder::new()
            .name("mining-boot".into())
            .spawn(move || {
                let user_id = TwitchClient::new(&client_id, &token)
                    .ok()
                    .and_then(|client| client.get_me().ok().flatten())
                    .map(|user| user.id)
                    .unwrap_or_default();
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                if user_id.is_empty() {
                    let _ = events.send(mining::Event::ClaimFailed(
                        "no se pudo obtener el ID de usuario de Twitch".into(),
                    ));
                    return;
                }
                let _ = mining::spawn(token, user_id, events, commands, stop);
            })
            .ok();
    }

    fn start_mining(&mut self, _ctx: &egui::Context) {
        if !self.has_creds() {
            self.set_toast("Configura el Client ID y el token para usar los puntos de canal");
            return;
        }
        self.ensure_mining_started();
    }

    fn mining_watch(&mut self, channel: &str) {
        let key = channel.trim().trim_start_matches('#').to_lowercase();
        if key.is_empty() || !self.has_creds() {
            return;
        }
        self.ensure_mining_started();
        let _ = self.mining_tx.send(mining::Command::Watch(key));
    }

    fn mining_unwatch(&mut self, channel: &str) {
        let key = channel.trim().trim_start_matches('#').to_lowercase();
        if key.is_empty() || !self.mining_running {
            return;
        }
        // Only stop heartbeats if the channel is no longer playing anywhere.
        let still_embedded = self.embedded_channel.as_deref() == Some(key.as_str());
        let still_external = self.running.keys().any(|k| k.eq_ignore_ascii_case(&key));
        if !still_embedded && !still_external {
            let _ = self.mining_tx.send(mining::Command::Unwatch(key));
        }
    }

    fn mining_refresh_channel(&mut self, channel: &str) {
        let key = channel.trim().trim_start_matches('#').to_lowercase();
        if key.is_empty() || !self.has_creds() {
            return;
        }
        self.ensure_mining_started();
        let _ = self.mining_tx.send(mining::Command::RefreshChannel(key));
    }

    fn stop_mining_internal(&mut self) {
        if let Some(stop) = self.mining_stop.take() {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let _ = self.mining_tx.send(mining::Command::Stop);
        self.mining_running = false;
        self.mining_connected = false;
        self.mining_auth_error = None;
    }

    fn stop_mining(&mut self) {
        self.stop_mining_internal();
        self.mining_log.push("Mining detenido".into());
        self.set_toast("Mining de puntos detenido");
    }

    fn handle_mining_event(&mut self, event: mining::Event) {
        match event {
            mining::Event::Connected(connected) => {
                let changed = self.mining_connected != connected;
                self.mining_connected = connected;
                if connected {
                    self.mining_auth_error = None;
                }
                if changed {
                    self.mining_log.push(if connected {
                        "Conectado a PubSub".into()
                    } else {
                        "PubSub desconectado".into()
                    });
                }
            }
            mining::Event::PubSubAuthRejected(err) => {
                self.mining_connected = false;
                if self.mining_auth_error.as_deref() != Some(err.as_str()) {
                    self.mining_auth_error = Some(err);
                    self.mining_log.push(
                        "PubSub rechazó el token (ERR_BADAUTH: requiere auth-token web de twitch.tv). Los heartbeats de visualización siguen activos.".into(),
                    );
                }
            }
            mining::Event::Points {
                channel,
                balance,
                earned,
            } => {
                let key = channel.to_lowercase();
                self.mining_points.insert(key.clone(), balance);
                if let Some(ctx) = self.mining_contexts.get_mut(&key) {
                    ctx.balance = Some(balance);
                }
                if earned > 0 {
                    self.mining_log
                        .push(format!("+{earned} pts en {key} (saldo {balance})"));
                }
            }
            mining::Event::ChannelContext { channel, context } => {
                let key = channel.to_lowercase();
                if let Some(balance) = context.balance {
                    self.mining_points.insert(key.clone(), balance);
                }
                self.mining_contexts.insert(key, context);
            }
            mining::Event::WatchTick { channel, ticks } => {
                let key = channel.to_lowercase();
                self.mining_watch_ticks.insert(key, ticks);
            }
            mining::Event::Chest(chest) => {
                self.mining_log.push(format!(
                    "Cofre disponible en {} (+{} pts)",
                    chest.channel, chest.points
                ));
                if !self.mining_chests.iter().any(|c| c.claim_id == chest.claim_id) {
                    self.mining_chests.push(chest);
                }
            }
            mining::Event::Claimed { channel, balance } => {
                let key = channel.to_lowercase();
                self.mining_chests
                    .retain(|chest| !chest.channel.eq_ignore_ascii_case(&key));
                if balance >= 0 {
                    self.mining_points.insert(key.clone(), balance as u64);
                    if let Some(ctx) = self.mining_contexts.get_mut(&key) {
                        ctx.balance = Some(balance as u64);
                    }
                }
                self.mining_log.push(format!("Cofre canjeado en {key}"));
                self.set_toast(format!("+{key}: cofre de puntos reclamado"));
            }
            mining::Event::ClaimFailed(error) => {
                self.mining_log.push(format!("No se pudo canjear: {error}"));
            }
            mining::Event::Redeemed {
                channel,
                reward_title,
                cost,
            } => {
                self.points_redeeming = false;
                self.points_selected_reward = None;
                self.points_reward_input.clear();
                self.mining_log.push(format!(
                    "Canjeado '{reward_title}' en {channel} (-{cost} pts)"
                ));
                self.set_toast(format!("Canjeado: {reward_title} (-{cost} pts)"));
            }
            mining::Event::RedeemFailed {
                channel,
                reward_title,
                error,
            } => {
                self.points_redeeming = false;
                self.mining_log.push(format!(
                    "Fallo al canjear '{reward_title}' en {channel}: {error}"
                ));
                self.set_toast(format!("{reward_title}: {error}"));
            }
            mining::Event::DropProgress {
                drop_id,
                channel,
                current,
                required,
            } => {
                self.mining_drops.insert(drop_id, (current, required));
                self.mining_log.push(format!(
                    "Drop {channel}: {current}/{required} minutos"
                ));
            }
            mining::Event::DropClaim { instance, channel } => {
                self.mining_log.push(format!("Drop reclamable en {channel} ({instance})"));
            }
        }
        const MAX_LOG: usize = 60;
        if self.mining_log.len() > MAX_LOG {
            let excess = self.mining_log.len() - MAX_LOG;
            self.mining_log.drain(0..excess);
        }
    }

    /// Bloque de puntos de canal (equivalente al de la web).
    fn mining_box(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = self.palette();
        egui::Frame::NONE
            .fill(p.surface)
            .corner_radius(egui::CornerRadius::same(12))
            .stroke(egui::Stroke::new(1.0, p.border))
            .inner_margin(egui::Margin::symmetric(14, 12))
            .show(ui, |ui| {
                let watching_any =
                    self.embedded_channel.is_some() || !self.running.is_empty();
                ui.horizontal(|ui| {
                    let active_heartbeats =
                        self.mining_running && self.mining_auth_error.is_some() && watching_any;
                    let dot = if self.mining_connected || active_heartbeats {
                        p.green
                    } else if self.mining_running {
                        p.gold
                    } else {
                        p.dim
                    };
                    let pulse = if self.mining_connected || active_heartbeats {
                        anim::pulse(ui.input(|input| input.time), 2.0)
                    } else {
                        1.0
                    };
                    ui.colored_label(theme::with_alpha(dot, pulse), "●");
                    let status_text = if !self.mining_running {
                        "Puntos de canal: desactivado"
                    } else if self.mining_connected {
                        "Puntos de canal: Conectado (PubSub)"
                    } else if self.mining_auth_error.is_some() {
                        if watching_any {
                            "Puntos de canal: Activo (heartbeats)"
                        } else {
                            "Puntos de canal: en espera (abre un stream)"
                        }
                    } else {
                        "Puntos de canal: conectando..."
                    };
                    ui.label(
                        egui::RichText::new(status_text)
                            .size(12.5)
                            .strong()
                            .color(p.text),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.mining_running {
                            if icons::wide_button(ui, "Detener", Icon::Stop, false, true, &p).clicked() {
                                self.stop_mining();
                            }
                        } else if icons::wide_button(ui, "Iniciar", Icon::Bolt, true, true, &p).clicked()
                        {
                            self.start_mining(ctx);
                        }
                    });
                });
                ui.add_space(6.0);

                let total: u64 = self.mining_points.values().sum();
                ui.label(
                    egui::RichText::new(format!("Saldo: {total} punto(s)"))
                        .size(11.5)
                        .color(p.muted),
                );
                if watching_any && self.mining_running {
                    let mut active_channels: Vec<String> = Vec::new();
                    if let Some(ref ch) = self.embedded_channel {
                        let ticks = self.mining_watch_ticks.get(&ch.to_lowercase()).copied().unwrap_or(0);
                        active_channels.push(if ticks > 0 {
                            format!("{ch} ({ticks} min)")
                        } else {
                            ch.clone()
                        });
                    }
                    for ch in self.running.keys() {
                        if self.embedded_channel.as_deref() == Some(ch.as_str()) {
                            continue;
                        }
                        let ticks = self.mining_watch_ticks.get(&ch.to_lowercase()).copied().unwrap_or(0);
                        active_channels.push(if ticks > 0 {
                            format!("{ch} ({ticks} min)")
                        } else {
                            ch.clone()
                        });
                    }
                    if !active_channels.is_empty() {
                        ui.label(
                            egui::RichText::new(format!("Viendo: {}", active_channels.join(", ")))
                                .size(11.0)
                                .color(p.green),
                        );
                    }
                }

                if !self.mining_chests.is_empty() {
                    ui.add_space(6.0);
                    for chest in self.mining_chests.clone() {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "Cofre en {} (+{} pts)",
                                    chest.channel, chest.points
                                ))
                                .size(11.5)
                                .color(p.gold),
                            );
                            if icons::wide_button(ui, "Reclamar", Icon::Gem, true, true, &p)
                                .clicked()
                            {
                                let _ = self.mining_tx.send(mining::Command::Claim(chest.clone()));
                            }
                        });
                    }
                }

                if !self.mining_drops.is_empty() {
                    ui.add_space(6.0);
                    for (drop_id, (current, required)) in self.mining_drops.clone() {
                        let ratio = if required > 0 {
                            current as f32 / required as f32
                        } else {
                            0.0
                        };
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!("Drop {drop_id}"))
                                    .size(11.0)
                                    .color(p.muted),
                            );
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(140.0, 8.0),
                                egui::Sense::hover(),
                            );
                            anim::progress_bar(
                                ui.painter(),
                                rect,
                                ratio,
                                p.gold,
                                p.accent,
                                p.surface3,
                            );
                            ui.label(
                                egui::RichText::new(format!("{current}/{required}"))
                                    .size(10.0)
                                    .color(p.dim)
                                    .monospace(),
                            );
                        });
                    }
                }

                if !self.mining_log.is_empty() {
                    ui.add_space(6.0);
                    for line in self.mining_log.iter().rev().take(5) {
                        ui.label(egui::RichText::new(line).size(10.5).color(p.dim));
                    }
                }
            });
    }

    // -- credenciales y herramientas ---------------------------------------

    fn save_token(&mut self) {
        let token = self.token_edit.trim().to_string();
        if token.is_empty() {
            self.set_toast("El token está vacío");
            return;
        }
        match config::save_token(&token) {
            Ok(()) => {
                self.token = token.clone();
                self.token_edit = token;
                self.followed_loaded = false;
                self.chat_identity();
                self.ensure_mining_started();
                self.set_toast("Token guardado en el llavero de Windows");
            }
            Err(err) => self.set_toast(format!("No se pudo guardar el token: {err:#}")),
        }
    }

    fn delete_token(&mut self) {
        match config::delete_token() {
            Ok(()) => {
                self.token.clear();
                self.token_edit.clear();
                self.set_toast("Token eliminado");
            }
            Err(err) => self.set_toast(format!("No se pudo eliminar el token: {err:#}")),
        }
    }

    fn check_tools(&mut self) {
        let mut messages = Vec::new();
        for (label, configured, fallback) in [
            ("mpv", self.cfg.mpv.as_str(), "mpv"),
            ("VLC", self.cfg.vlc.as_str(), "vlc"),
            ("Streamlink", self.cfg.streamlink.as_str(), "streamlink"),
        ] {
            match player::resolve_tool(configured, fallback, "") {
                Ok(path) => messages.push(format!("{label}: {}", path.display())),
                Err(_) => messages.push(format!("{label}: NO encontrado")),
            }
        }
        self.set_toast(messages.join("  |  "));
    }
}

impl eframe::App for StreamHubApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut any_event = false;
        while let Ok(event) = self.rx.try_recv() {
            any_event = true;
            self.handle_event(event);
        }
        // Chat: los eventos llegan del hilo de IRC.
        while let Ok(event) = self.chat_rx.try_recv() {
            any_event = true;
            self.handle_chat_event(event);
        }
        // Puntos de canal: eventos de PubSub y GQL.
        while let Ok(event) = self.mining_rx.try_recv() {
            any_event = true;
            self.handle_mining_event(event);
        }
        // 7TV en vivo.
        while let Ok(event) = self.sevent_rx.try_recv() {
            any_event = true;
            self.handle_event(Event::Seven(event));
        }
        if any_event {
            ctx.request_repaint();
        }
        if let Some(release) = self.pending_auto_update.take() {
            self.spawn_update_download(ctx, release);
        }

        // Detectar reproductores que terminaron por sí solos.
        let mut finished = Vec::new();
        for (name, stream) in self.running.iter_mut() {
            match stream.child.try_wait() {
                Ok(Some(status)) => finished.push((name.clone(), format!("{status}"))),
                Ok(None) => {}
                Err(err) => finished.push((name.clone(), format!("error: {err}"))),
            }
        }
        for (name, status) in finished {
            self.running.remove(&name);
            self.mining_unwatch(&name);
            self.set_toast(format!("{name}: el reproductor terminó ({status})"));
        }

        if self.pending_live_refresh && !self.busy_live {
            self.pending_live_refresh = false;
            self.spawn_live(ctx);
        }

        if self.has_creds()
            && !self.busy_live
            && self.last_live_refresh.elapsed() > Duration::from_secs(60)
            && (!self.queue.is_empty() || !self.cfg.favorites.is_empty() || !self.followed.is_empty())
        {
            self.spawn_live(ctx);
        }

        if self.logs_open && self.logs_updated.elapsed() > Duration::from_millis(900) {
            self.refresh_logs();
        }

        if let Some(player) = &self.embedded {
            player.poll(ctx);
        }

        // Atajos de teclado globales (solo si ningún campo de texto tiene foco).
        let typing = ctx.egui_wants_keyboard_input();
        let no_mods = ctx.input(|i| !i.modifiers.any());
        if !typing {
            if no_mods {
                if let Some(player) = &self.embedded {
                    if ctx.input(|i| i.key_pressed(egui::Key::Space)) {
                        player.toggle_pause();
                        ctx.request_repaint();
                    }
                    if ctx.input(|i| i.key_pressed(egui::Key::M)) {
                        player.toggle_mute();
                        ctx.request_repaint();
                    }
                    if ctx.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
                        let snap = player.snapshot();
                        player.set_volume(snap.volume.saturating_add(5));
                        ctx.request_repaint();
                    }
                    if ctx.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                        let snap = player.snapshot();
                        player.set_volume(snap.volume.saturating_sub(5));
                        ctx.request_repaint();
                    }
                    if ctx.input(|i| i.key_pressed(egui::Key::F)) {
                        let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
                    }
                }
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && self.chat_panel {
                self.chat_panel = false;
                ctx.request_repaint();
            }
        }

        // Repintar inmediatamente solo si llegaron eventos; el reproductor
        // integrado (callback de libmpv), el toast (40ms) y las animaciones
        // ya programan sus propios repintados bajo demanda.
        if any_event {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.gl = frame.gl().cloned();
        let p = self.palette();
        // Degradado de fondo de la referencia (capa inferior, detrás de todo).
        theme::paint_background(&ctx, &p);
        self.images.poll(&ctx);
        self.top_bar(ui, &ctx);
        self.bottom_bar(ui);
        self.side_bar(ui, &ctx);
        self.chat_panel(ui, &ctx);
        // Transición de pestañas: fade + pequeño deslizamiento al entrar.
        if self.tab != self.last_tab {
            self.last_tab = self.tab;
            self.tab_at = Instant::now();
        }
        let tab_t = (self.tab_at.elapsed().as_secs_f32() / 0.18).clamp(0.0, 1.0);
        let tab_slide = anim::ease(tab_t, anim::Easing::EaseOut);
        if tab_t < 1.0 {
            ctx.request_repaint();
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(egui::Color32::TRANSPARENT)
                    .inner_margin(egui::Margin::symmetric(18, 14)),
            )
            .show(ui, |ui| {
                if tab_slide < 1.0 {
                    ui.set_opacity(tab_slide);
                    ui.add_space((1.0 - tab_slide) * 10.0);
                }
                match self.tab {
                    Tab::Player => self.tab_player(ui, &ctx),
                    Tab::Queue => self.tab_queue(ui, &ctx),
                    Tab::Favorites => self.tab_favorites(ui, &ctx),
                    Tab::Followed => self.tab_followed(ui, &ctx),
                    Tab::Explore => self.tab_explore(ui, &ctx),
                    Tab::Settings => self.tab_settings(ui, &ctx),
                }
            });
        self.logs_window(&ctx);
        self.toast_overlay(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_state();
        let _ = self.cfg.save();
        self.shutdown();
    }
}

impl Drop for StreamHubApp {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ---------------------------------------------------------------------------
// Utilidades libres
// ---------------------------------------------------------------------------

/// Extrae el login de un canal a partir de un nombre o de una URL de Twitch.
fn parse_channel(input: &str) -> Option<String> {
    let text = input.trim();
    if text.is_empty() {
        return None;
    }
    let lower = text.to_lowercase();
    if lower.contains("twitch.tv/") {
        let rest = lower.split("twitch.tv/").nth(1)?;
        let name = rest.split(['/', '?', '#']).next().unwrap_or("").trim();
        if name.is_empty() || matches!(name, "videos" | "directory" | "p" | "settings") {
            return None;
        }
        return Some(name.to_string());
    }
    let name = text
        .trim_start_matches('@')
        .trim_start_matches('#')
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_lowercase();
    if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        Some(name)
    } else {
        None
    }
}

/// Número de columnas que caben en `width` con un ancho mínimo por tarjeta.
fn grid_columns(width: f32, min_cell: f32) -> usize {
    ((width / min_cell).floor() as usize).clamp(1, 8)
}

/// Anillo giratorio (indicador de carga/búfer) dibujado con puntos.
fn spinner(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    time: f64,
    color: egui::Color32,
) {
    const DOTS: usize = 8;
    let head = (time as f32 * 4.5) % std::f32::consts::TAU;
    for i in 0..DOTS {
        let angle = head - i as f32 * (std::f32::consts::TAU / DOTS as f32);
        let fade = 1.0 - i as f32 / DOTS as f32;
        let pos = center + egui::vec2(angle.cos(), angle.sin()) * radius;
        painter.circle_filled(
            pos,
            1.6 + radius * 0.06,
            theme::with_alpha(color, 0.15 + 0.85 * fade),
        );
    }
}

fn section_title(ui: &mut egui::Ui, title: &str, count: usize, p: &Palette) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(title)
                .size(14.0)
                .strong()
                .color(p.text),
        );
        let text = count.to_string();
        let font = egui::FontId::new(9.5, egui::FontFamily::Monospace);
        let galley = ui.painter().layout_no_wrap(text, font, p.muted);
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2((galley.size().x + 12.0).max(22.0), 18.0),
            egui::Sense::hover(),
        );
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(9), p.surface2);
        ui.painter().galley(
            egui::pos2(
                rect.center().x - galley.size().x / 2.0,
                rect.center().y - galley.size().y / 2.0,
            ),
            galley,
            p.muted,
        );
    });
    ui.add_space(6.0);
}

fn settings_section(
    ui: &mut egui::Ui,
    title: &str,
    default_open: bool,
    p: &Palette,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    // Tarjeta plegable: la cabecera anima el despliegue y el scroll de
    // Ajustes deja de ser interminable.
    egui::Frame::NONE
        .fill(p.surface)
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            egui::CollapsingHeader::new(
                egui::RichText::new(title)
                    .monospace()
                    .size(9.5)
                    .color(p.muted),
            )
            .id_salt(("settings_section", title))
            .default_open(default_open)
            .show(ui, add_contents);
        });
}

fn ellipsize(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn initials(name: &str) -> String {
    let mut chars = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(2)
        .collect::<String>();
    if chars.is_empty() {
        chars = "?".to_string();
    }
    chars.to_uppercase()
}

fn badge(ui: &mut egui::Ui, text: &str, fg: egui::Color32, bg: egui::Color32) {
    let font = egui::FontId::new(8.5, egui::FontFamily::Monospace);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, fg);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(galley.size().x + 12.0, galley.size().y + 5.0),
        egui::Sense::hover(),
    );
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(4), bg);
    ui.painter().galley(
        egui::pos2(
            rect.min.x + 6.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        fg,
    );
}

/// Chip de filtro (Todos / En vivo / Sin emisión).
fn filter_chip(ui: &mut egui::Ui, text: &str, active: bool, p: &Palette) -> egui::Response {
    let font = egui::FontId::new(11.5, egui::FontFamily::Proportional);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), font, p.muted);
    let size = egui::vec2(galley.size().x + 22.0, 27.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let hover_t = ui.ctx().animate_bool_with_time(
            egui::Id::unique(("filter_chip", text)),
            response.hovered(),
            0.12,
        );
        let bg = if active {
            p.accent_weak
        } else {
            theme::lerp_color(egui::Color32::TRANSPARENT, p.surface2, hover_t)
        };
        let border = if active {
            p.accent_mid
        } else if response.hovered() {
            p.border2
        } else {
            p.border
        };
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(14), bg);
        ui.painter().rect_stroke(
            rect,
            egui::CornerRadius::same(14),
            egui::Stroke::new(1.0, border),
            egui::StrokeKind::Inside,
        );
        let color = if active { p.accent } else { p.muted };
        ui.painter().galley(
            egui::pos2(
                rect.center().x - galley.size().x / 2.0,
                rect.center().y - galley.size().y / 2.0,
            ),
            galley,
            color,
        );
    }
    response
}

/// Icono y color del toast según el mensaje (tabla de la web).
fn toast_icon(message: &str) -> (Option<Icon>, egui::Color32) {
    let text = message.to_lowercase();
    let red = egui::Color32::from_rgb(224, 85, 58);
    let green = egui::Color32::from_rgb(63, 184, 90);
    let gold = egui::Color32::from_rgb(240, 168, 48);
    let blue = egui::Color32::from_rgb(58, 138, 240);
    if text.contains("error") || text.contains("fallo") || text.contains("fail") {
        (Some(Icon::Close), red)
    } else if text.contains("mining") || text.contains("puntos") {
        (Some(Icon::Bolt), gold)
    } else if text.contains("drop") || text.contains("reward") {
        (Some(Icon::Gift), gold)
    } else if text.contains("grid") {
        (Some(Icon::Grid), blue)
    } else if text.contains("ok")
        || text.contains("éxito")
        || text.contains("exito")
        || text.contains("iniciado")
        || text.contains("activado")
        || text.contains("guardado")
        || text.contains("detenido")
        || text.contains("migrado")
    {
        (Some(Icon::Check), green)
    } else {
        (None, egui::Color32::WHITE)
    }
}

/// Convierte `#RRGGBB` en un color de egui.
fn parse_hex_color(hex: &str) -> Option<egui::Color32> {
    let hex = hex.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(egui::Color32::from_rgb(r, g, b))
}

/// Etiqueta corta de los badges del chat (MOD, SUB, VIP...).
#[allow(dead_code)]
fn badge_label(badge: &str) -> Option<(&'static str, egui::Color32)> {
    let (kind, _version) = badge.split_once('/').unwrap_or((badge, "1"));
    Some(match kind {
        "moderator" => ("MOD", egui::Color32::from_rgb(74, 222, 123)),
        "broadcaster" => ("OWNER", egui::Color32::from_rgb(245, 101, 101)),
        "vip" => ("VIP", egui::Color32::from_rgb(251, 125, 220)),
        "artist-badge" => ("ARTIST", egui::Color32::from_rgb(251, 125, 220)),
        "founder" => ("FOUNDER", egui::Color32::from_rgb(240, 192, 64)),
        "subscriber" => ("SUB", egui::Color32::from_rgb(96, 165, 250)),
        "turbo" => ("TURBO", egui::Color32::from_rgb(96, 165, 250)),
        "premium" => ("PRIME", egui::Color32::from_rgb(96, 165, 250)),
        "no_audio" => ("NO AUDIO", egui::Color32::from_rgb(120, 120, 140)),
        "bits" => ("BITS", egui::Color32::from_rgb(240, 192, 64)),
        "warlord" => ("WARLORD", egui::Color32::from_rgb(240, 100, 140)),
        "wholesome-direct-2026" => ("SUPPORT", egui::Color32::from_rgb(140, 200, 240)),
        "rplace-2023" => ("RPLACE", egui::Color32::from_rgb(140, 200, 240)),
        _ => return None,
    })
}

fn push_login(logins: &mut Vec<String>, value: &str) {
    let value = value.trim().to_lowercase();
    if !value.is_empty() && !logins.iter().any(|existing| existing == &value) {
        logins.push(value);
    }
}

fn fetch_followed(client_id: &str, token: &str) -> Result<FollowedData, String> {
    let client = TwitchClient::new(client_id, token).map_err(|err| format!("{err:#}"))?;
    let me = client
        .get_me()
        .map_err(|err| format!("{err:#}"))?
        .ok_or_else(|| "El token no pertenece a ningún usuario de Twitch".to_string())?;
    let channels = client
        .get_followed(&me.id)
        .map_err(|err| format!("{err:#}"))?;
    Ok(FollowedData {
        user: Some(me),
        channels,
    })
}

fn grid_geometry(ctx: &egui::Context, index: usize, total: usize) -> Geometry {
    let (width_points, height_points) = ctx
        .input(|input| input.viewport().monitor_size)
        .map(|size| (size.x, size.y))
        .unwrap_or((1600.0, 900.0));
    let pixels_per_point = ctx.pixels_per_point().max(0.5);
    let screen_width = (width_points * pixels_per_point).round().max(640.0) as u32;
    let screen_height = (height_points * pixels_per_point).round().max(480.0) as u32;

    let columns = (total as f64).sqrt().ceil().max(1.0) as u32;
    let rows = (total as u32).div_ceil(columns).max(1);
    let width = (screen_width / columns).max(320);
    let height = (screen_height / rows).max(240);
    let column = (index as u32) % columns;
    let row = (index as u32) / columns;

    Geometry {
        x: (column * width) as i32,
        y: (row * height) as i32,
        width,
        height,
    }
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds >= 3600 {
        format!("{}h {:02}m", seconds / 3600, (seconds % 3600) / 60)
    } else if seconds >= 60 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

fn format_count(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1} M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1} k", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

/// Convierte la lista de un proveedor al par (nombre, URL) que usa la UI.
fn to_pairs(list: &[emotes::ExternalEmote]) -> Vec<(String, String)> {
    list.iter()
        .map(|emote| (emote.name.clone(), emote.url.clone()))
        .collect()
}

/// Texto relativo para la fecha de la caché (`hace 3 min`).
fn format_age(updated: u64) -> String {
    let secs = emote_cache::now_secs().saturating_sub(updated);
    match secs {
        0..=59 => "hace un momento".to_string(),
        60..=3_599 => format!("hace {} min", secs / 60),
        3_600..=86_399 => format!("hace {} h", secs / 3_600),
        _ => format!("hace {} días", secs / 86_400),
    }
}

fn path_row(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.label(label);
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(value).desired_width(380.0));
        if ui.button("...").clicked() {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Ejecutable", &["exe"])
                .pick_file()
            {
                *value = path.display().to_string();
            }
        }
    });
    ui.end_row();
}

fn open_path(path: &std::path::Path) {
    let _ = std::process::Command::new("explorer").arg(path).spawn();
}

fn open_url(url: &str) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

/// Renders numbered pagination controls (`<  1  2  3  ...  >`) and returns
/// the requested 0-based page index when the user clicks a page tab.
fn pagination_bar(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    current: usize,
    loaded_pages: usize,
    has_more: bool,
    busy: bool,
    p: &Palette,
) -> Option<usize> {
    let total_tabs = loaded_pages + usize::from(has_more);
    if total_tabs <= 1 && !busy {
        return None;
    }

    let mut requested: Option<usize> = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;

        let can_prev = current > 0 && !busy;
        if ui
            .add_enabled(
                can_prev,
                egui::Button::new(egui::RichText::new("‹ Anterior").size(12.0).color(p.text))
                    .fill(p.surface2)
                    .stroke(egui::Stroke::new(1.0, p.border))
                    .corner_radius(egui::CornerRadius::same(8)),
            )
            .clicked()
        {
            requested = Some(current.saturating_sub(1));
        }

        // Build visible page indices with ellipsis when many pages are loaded.
        let mut visible: Vec<Option<usize>> = Vec::new();
        if total_tabs <= 7 {
            for idx in 0..total_tabs {
                visible.push(Some(idx));
            }
        } else {
            visible.push(Some(0));
            let start = current.saturating_sub(1).max(1);
            let end = (current + 1).min(total_tabs - 2);
            if start > 1 {
                visible.push(None);
            }
            for idx in start..=end {
                visible.push(Some(idx));
            }
            if end < total_tabs - 2 {
                visible.push(None);
            }
            visible.push(Some(total_tabs - 1));
        }

        for entry in visible {
            match entry {
                None => {
                    ui.label(egui::RichText::new("...").size(12.0).color(p.muted));
                }
                Some(idx) => {
                    let is_active = idx == current;
                    let is_next_unloaded = idx == loaded_pages && has_more;
                    let fill = if is_active { p.accent } else { p.surface2 };
                    let text_color = if is_active { p.on_accent } else { p.text };
                    let stroke = if is_active {
                        egui::Stroke::NONE
                    } else {
                        egui::Stroke::new(1.0, p.border)
                    };
                    let btn = egui::Button::new(
                        egui::RichText::new(format!("{}", idx + 1))
                            .size(12.0)
                            .strong()
                            .color(text_color),
                    )
                    .min_size(egui::vec2(30.0, 26.0))
                    .fill(fill)
                    .stroke(stroke)
                    .corner_radius(egui::CornerRadius::same(8));
                    let resp = ui.add_enabled(!busy, btn);
                    let resp = if is_next_unloaded {
                        resp.on_hover_text("Cargar siguiente página")
                    } else {
                        resp
                    };
                    if resp.clicked() && !is_active {
                        requested = Some(idx);
                    }
                }
            }
        }

        if has_more {
            ui.label(egui::RichText::new("...").size(12.0).color(p.muted));
        }

        let can_next = !busy && (current + 1 < loaded_pages || has_more);
        if ui
            .add_enabled(
                can_next,
                egui::Button::new(egui::RichText::new("Siguiente ›").size(12.0).color(p.text))
                    .fill(p.surface2)
                    .stroke(egui::Stroke::new(1.0, p.border))
                    .corner_radius(egui::CornerRadius::same(8)),
            )
            .clicked()
        {
            requested = Some(current + 1);
        }

        if busy {
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
            spinner(ui.painter(), rect.center(), 7.0, ctx.input(|i| i.time), p.accent);
        }
    });

    requested
}

/// Muestra una notificación nativa de Windows (toast) usando PowerShell.
fn show_windows_notification(title: &str, body: &str) {
    use std::os::windows::process::CommandExt;
    let title_escaped = title.replace("'", "''");
    let body_escaped = body.replace("'", "''");
    let ps = format!(
        "$t=[Windows.UI.Notifications.ToastTemplateType]::ToastText02; \
         $x=[Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent($t); \
         $y=$x.GetElementsByTagName('text'); \
         $y[0].AppendChild($x.CreateTextNode('{}')); \
         $y[1].AppendChild($x.CreateTextNode('{}')); \
         $n=[Windows.UI.Notifications.ToastNotification]::new($x); \
         [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('StreamHub').Show($n)",
        title_escaped, body_escaped
    );
    let _ = std::process::Command::new("powershell")
        .args(["-Command", &ps])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .spawn();
}

#[cfg(windows)]
fn load_logo(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let bytes = include_bytes!("../assets/logo.png");
    let image = image::load_from_memory(bytes).ok()?.to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, &image.into_raw());
    Some(ctx.load_texture("streamhub-logo", color, egui::TextureOptions::LINEAR))
}

pub fn load_icon() -> std::sync::Arc<egui::IconData> {
    let bytes = include_bytes!("../assets/logo.png");
    if let Ok(image) = image::load_from_memory(bytes) {
        let image = image.to_rgba8();
        let (width, height) = image.dimensions();
        return std::sync::Arc::new(egui::IconData {
            rgba: image.into_raw(),
            width,
            height,
        });
    }
    std::sync::Arc::new(egui::IconData {
        rgba: vec![0; 4 * 32 * 32],
        width: 32,
        height: 32,
    })
}
