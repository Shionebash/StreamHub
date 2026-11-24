//! Reproducción integrada del stream con libmpv + OpenGL (backend glow).
//!
//! libmpv decodifica el HLS que resuelve Streamlink y entrega los fotogramas
//! en una textura OpenGL propia (FBO). egui pinta esa textura con un
//! `PaintCallback`, sin copiar vídeo a la CPU. Solo se admite un stream
//! integrado a la vez para mantener el consumo de memoria bajo.

use crate::{config::Config, player};
use eframe::{egui, egui_glow, glow};
use egui::{mutex::Mutex, Context, PaintCallback, Rect, Ui};
use glow::HasContext as _;
use libmpv2::{
    events::Event,
    mpv_end_file_reason,
    render::{OpenGLInitParams, RenderContext, RenderParam, RenderParamApiType},
    Mpv,
};
use std::{
    ffi::{c_void, CString},
    num::NonZeroU32,
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};

/// Calidades ofrecidas por el reproductor integrado (la primera es "Auto").
pub const QUALITIES: [&str; 7] = [
    "best", "1080p60", "720p60", "720p", "480p", "360p", "worst",
];

pub fn quality_label(quality: &str) -> &str {
    match quality {
        "best" => "Auto",
        other => other,
    }
}

#[derive(Clone, PartialEq, Debug, Default)]
pub enum Phase {
    #[default]
    Resolving,
    Loading,
    Playing,
    Buffering,
    Offline,
    Error(String),
}

impl Phase {
    pub fn label(&self) -> String {
        match self {
            Self::Resolving => "Resolviendo stream...".to_string(),
            Self::Loading => "Cargando vídeo...".to_string(),
            Self::Playing => "En directo".to_string(),
            Self::Buffering => "Almacenando en búfer...".to_string(),
            Self::Offline => "El canal está sin emisión".to_string(),
            Self::Error(err) => err.clone(),
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self, Self::Playing | Self::Buffering)
    }
}

#[derive(Clone)]
pub struct Snapshot {
    pub channel: String,
    pub phase: Phase,
    pub paused: bool,
    pub muted: bool,
    pub volume: i64,
    pub quality: String,
    pub audio_only: bool,
}

enum Resolved {
    Url(String),
    Offline,
    Err(String),
}

#[cfg(windows)]
fn gl_proc(_: &(), name: &str) -> *mut c_void {
    use libloading::{Library, Symbol};
    use std::sync::OnceLock;
    static LIB: OnceLock<Library> = OnceLock::new();
    let lib = LIB.get_or_init(|| unsafe { Library::new("opengl32.dll").expect("opengl32.dll") });
    let cname = CString::new(name).unwrap_or_default();
    unsafe {
        let wgl: Symbol<unsafe extern "system" fn(*const std::os::raw::c_char) -> *mut c_void> =
            lib.get(b"wglGetProcAddress\0").expect("wglGetProcAddress");
        let ptr = wgl(cname.as_ptr());
        let address = ptr as usize;
        if address > 3 && address != usize::MAX {
            return ptr;
        }
        lib.get::<*mut c_void>(cname.as_bytes_with_nul())
            .map(|symbol| *symbol)
            .unwrap_or(std::ptr::null_mut())
    }
}

#[cfg(not(windows))]
fn gl_proc(_: &(), _name: &str) -> *mut c_void {
    std::ptr::null_mut()
}

struct Fbo {
    fb: glow::Framebuffer,
    tex: glow::Texture,
    size: [i32; 2],
}

struct Inner {
    // El contexto de render toma prestado el `Mpv`; se declara primero para
    // que se libere antes que el reproductor. El préstamo se borra con
    // `transmute` porque ambos viven en el mismo `Arc` y nunca se mueven.
    render: RenderContext<'static>,
    mpv: Box<Mpv>,
    fbo: Option<Fbo>,
    closed: bool,
    channel: String,
    quality: String,
    token: String,
    streamlink: String,
    audio_only: bool,
    volume: i64,
    paused: bool,
    muted: bool,
    phase: Phase,
    url: Option<String>,
    rx: Option<mpsc::Receiver<Resolved>>,
    last_poll: Instant,
}

// El reproductor solo se usa desde el hilo de la interfaz. El `Arc<Mutex>`
// viaja al callback de pintado, que egui ejecuta en ese mismo hilo.
unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}

/// Reproductor integrado compartido entre la lógica de la app y el callback
/// de pintado de egui.
#[derive(Clone)]
pub struct EmbeddedPlayer(Arc<Mutex<Inner>>);

impl EmbeddedPlayer {
    pub fn new(
        ctx: &Context,
        cfg: &Config,
        token: &str,
        channel: &str,
        audio_only: bool,
    ) -> Result<Self, String> {
        let mpv = Mpv::with_initializer(|init| {
            init.set_property("vo", "libmpv")?;
            init.set_property("hwdec", "auto-safe")?;
            init.set_property("osc", false)?;
            init.set_property("input-default-bindings", false)?;
            init.set_property("input-vo-keyboard", false)?;
            init.set_property("osd-level", 0)?;
            init.set_property("ytdl", false)?;
            init.set_property("audio-display", "no")?;
            init.set_property("demuxer-max-bytes", "32MiB")?;
            init.set_property("demuxer-max-back-bytes", "8MiB")?;
            init.set_property("cache-secs", 10.0)?;
            init.set_property("volume", 70_i64)?;
            init.set_property("mute", false)?;
            init.set_property("pause", false)?;
            Ok(())
        })
        .map_err(|err| format!("no se pudo crear libmpv: {err}"))?;

        if audio_only {
            let _ = mpv.set_property("vid", "no");
        }

        let mpv = Box::new(mpv);
        let render = mpv
            .create_render_context::<()>(vec![
                RenderParam::ApiType(RenderParamApiType::OpenGl),
                RenderParam::InitParams(OpenGLInitParams {
                    get_proc_address: gl_proc,
                    ctx: (),
                }),
            ])
            .map_err(|err| format!("no se pudo crear el contexto OpenGL de libmpv: {err}"))?;
        // SAFETY: `mpv` está en un `Box` y ambos campos viven y se liberan
        // juntos en `Inner`; el contexto de render se declara antes para
        // destruirse primero.
        let mut render: RenderContext<'static> = unsafe { std::mem::transmute(render) };
        let repaint = ctx.clone();
        render.set_update_callback(move || repaint.request_repaint());

        let player = Self(Arc::new(Mutex::new(Inner {
            render,
            mpv,
            fbo: None,
            closed: false,
            channel: channel.to_string(),
            quality: cfg.quality.clone(),
            token: token.to_string(),
            streamlink: cfg.streamlink.clone(),
            audio_only,
            volume: 70,
            paused: false,
            muted: false,
            phase: Phase::Resolving,
            url: None,
            rx: None,
            last_poll: Instant::now(),
        })));

        player.resolve(ctx);
        Ok(player)
    }

    fn resolve(&self, ctx: &Context) {
        let mut inner = self.0.lock();
        inner.phase = Phase::Resolving;
        let token = inner.token.clone();
        let streamlink = inner.streamlink.clone();
        let channel = inner.channel.clone();
        let quality = inner.quality.clone();
        inner.last_poll = Instant::now();
        inner.rx = Some(spawn_resolver(
            ctx.clone(),
            token,
            streamlink,
            channel,
            quality,
        ));
    }

    pub fn snapshot(&self) -> Snapshot {
        let inner = self.0.lock();
        Snapshot {
            channel: inner.channel.clone(),
            phase: inner.phase.clone(),
            paused: inner.paused,
            muted: inner.muted,
            volume: inner.volume,
            quality: inner.quality.clone(),
            audio_only: inner.audio_only,
        }
    }

    pub fn toggle_pause(&self) {
        let mut inner = self.0.lock();
        inner.paused = !inner.paused;
        let paused = inner.paused;
        let _ = inner.mpv.set_property("pause", paused);
    }

    pub fn toggle_mute(&self) {
        let mut inner = self.0.lock();
        inner.muted = !inner.muted;
        let muted = inner.muted;
        let _ = inner.mpv.set_property("mute", muted);
    }

    pub fn set_volume(&self, volume: i64) {
        let mut inner = self.0.lock();
        inner.volume = volume.clamp(0, 100);
        inner.muted = false;
        let volume = inner.volume;
        let _ = inner.mpv.set_property("volume", volume);
        let _ = inner.mpv.set_property("mute", false);
    }

    pub fn set_video(&self, on: bool) {
        let mut inner = self.0.lock();
        inner.audio_only = !on;
        let _ = inner.mpv.set_property("vid", if on { "auto" } else { "no" });
    }

    pub fn set_quality(&self, ctx: &Context, quality: &str) {
        let mut inner = self.0.lock();
        if inner.quality == quality {
            return;
        }
        inner.quality = quality.to_string();
        drop(inner);
        self.resolve(ctx);
    }

    /// Vuelve a resolver la URL del canal (reintento tras un error u offline).
    pub fn reload(&self, ctx: &Context) {
        self.resolve(ctx);
    }

    /// Procesa eventos y el estado de búfer. Se llama una vez por frame.
    pub fn poll(&self, ctx: &Context) {
        let mut inner = self.0.lock();
        #[cfg(debug_assertions)]
        let before = inner.phase.clone();

        if let Some(rx) = &inner.rx {
            if let Ok(result) = rx.try_recv() {
                inner.rx = None;
                match result {
                    Resolved::Url(url) => {
                        inner.phase = Phase::Loading;
                        inner.url = Some(url.clone());
                        let _ = inner.mpv.set_property("pause", false);
                        inner.paused = false;
                        let _ = inner.mpv.command("loadfile", &[&url, "replace"]);
                    }
                    Resolved::Offline => inner.phase = Phase::Offline,
                    Resolved::Err(err) => inner.phase = Phase::Error(err),
                }
            }
        }

        while let Some(event) = inner.mpv.wait_event(0.0) {
            match event {
                Ok(Event::FileLoaded) => inner.phase = Phase::Playing,
                Ok(Event::PlaybackRestart) => {
                    if !matches!(inner.phase, Phase::Error(_) | Phase::Offline) {
                        inner.phase = Phase::Playing;
                    }
                }
                Ok(Event::EndFile(reason)) => {
                    if matches!(
                        inner.phase,
                        Phase::Loading | Phase::Playing | Phase::Buffering
                    ) {
                        inner.phase = if reason == mpv_end_file_reason::Eof {
                            Phase::Offline
                        } else {
                            Phase::Error("El stream terminó o se cortó".to_string())
                        };
                    }
                }
                Ok(Event::Shutdown) => {
                    inner.phase = Phase::Error("libmpv se cerró".to_string());
                }
                Ok(_) => {}
                Err(err) => {
                    inner.phase = Phase::Error(format!("Error de libmpv: {err}"));
                }
            }
        }

        if matches!(inner.phase, Phase::Playing | Phase::Buffering) {
            if inner.last_poll.elapsed() >= Duration::from_millis(250) {
                inner.last_poll = Instant::now();
                let buffering: bool = inner
                    .mpv
                    .get_property("paused-for-cache")
                    .unwrap_or(false);
                inner.phase = if buffering {
                    Phase::Buffering
                } else {
                    Phase::Playing
                };
                if let Ok(paused) = inner.mpv.get_property::<bool>("pause") {
                    inner.paused = paused;
                }
                if let Ok(muted) = inner.mpv.get_property::<bool>("mute") {
                    inner.muted = muted;
                }
            }
            ctx.request_repaint_after(Duration::from_millis(250));
        } else if matches!(
            inner.phase,
            Phase::Resolving | Phase::Loading | Phase::Buffering
        ) {
            ctx.request_repaint_after(Duration::from_millis(250));
        }

        #[cfg(debug_assertions)]
        if before != inner.phase && std::env::var("SH_DEBUG").is_ok() {
            eprintln!("[embedded] {} -> {}", inner.channel, inner.phase.label());
        }
    }

    /// Pinta el vídeo dentro de `rect` usando una textura OpenGL propia.
    pub fn paint(&self, ui: &Ui, rect: Rect) {
        if rect.width() <= 1.0 || rect.height() <= 1.0 {
            return;
        }
        let shared = self.0.clone();
        ui.painter().add(PaintCallback {
            rect,
            callback: Arc::new(egui_glow::CallbackFn::new(move |info, painter| {
                let gl = painter.gl();
                let viewport = info.viewport_in_pixels();
                let width = viewport.width_px;
                let height = viewport.height_px;
                if width <= 0 || height <= 0 {
                    return;
                }
                let mut inner = shared.lock();
                if inner.closed {
                    return;
                }
                unsafe {
                    // Hay que capturar el framebuffer de egui *antes* de tocar
                    // el nuestro; si no, se guardaría el FBO propio y egui
                    // acabaría pintando dentro de él (pantalla en negro).
                    let previous = gl.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING);
                    let previous_fb = NonZeroU32::new(previous as u32).map(glow::NativeFramebuffer);

                    if inner.fbo.as_ref().map(|fbo| fbo.size) != Some([width, height]) {
                        if let Some(old) = inner.fbo.take() {
                            gl.delete_framebuffer(old.fb);
                            gl.delete_texture(old.tex);
                        }
                        let tex = match gl.create_texture() {
                            Ok(tex) => tex,
                            Err(_) => return,
                        };
                        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                        gl.tex_image_2d(
                            glow::TEXTURE_2D,
                            0,
                            glow::RGBA8 as i32,
                            width,
                            height,
                            0,
                            glow::RGBA,
                            glow::UNSIGNED_BYTE,
                            glow::PixelUnpackData::Slice(None),
                        );
                        gl.tex_parameter_i32(
                            glow::TEXTURE_2D,
                            glow::TEXTURE_MIN_FILTER,
                            glow::LINEAR as i32,
                        );
                        gl.tex_parameter_i32(
                            glow::TEXTURE_2D,
                            glow::TEXTURE_MAG_FILTER,
                            glow::LINEAR as i32,
                        );
                        let fb = match gl.create_framebuffer() {
                            Ok(fb) => fb,
                            Err(_) => return,
                        };
                        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fb));
                        gl.framebuffer_texture_2d(
                            glow::FRAMEBUFFER,
                            glow::COLOR_ATTACHMENT0,
                            glow::TEXTURE_2D,
                            Some(tex),
                            0,
                        );
                        gl.bind_texture(glow::TEXTURE_2D, None);
                        // Devolver el framebuffer de egui tras crear el FBO.
                        gl.bind_framebuffer(glow::FRAMEBUFFER, previous_fb);
                        inner.fbo = Some(Fbo {
                            fb,
                            tex,
                            size: [width, height],
                        });
                    }
                    let Some(fbo) = inner.fbo.as_ref() else {
                        return;
                    };

                    let _ = inner
                        .render
                        .render::<()>(fbo.fb.0.get() as i32, width, height, true);

                    gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(fbo.fb));
                    gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, previous_fb);
                    gl.blit_framebuffer(
                        0,
                        0,
                        width,
                        height,
                        viewport.left_px,
                        viewport.from_bottom_px,
                        viewport.left_px + width,
                        viewport.from_bottom_px + height,
                        glow::COLOR_BUFFER_BIT,
                        glow::LINEAR,
                    );
                    gl.bind_framebuffer(glow::FRAMEBUFFER, previous_fb);
                }
            })),
        });
    }

    /// Marca el reproductor como cerrado (ningún callback volverá a pintar).
    pub fn close(&self) {
        self.0.lock().closed = true;
    }

    /// Libera la textura y el framebuffer con el contexto GL todavía activo.
    pub fn destroy(&self, gl: &glow::Context) {
        let mut inner = self.0.lock();
        inner.closed = true;
        if let Some(fbo) = inner.fbo.take() {
            unsafe {
                gl.delete_framebuffer(fbo.fb);
                gl.delete_texture(fbo.tex);
            }
        }
    }
}

fn spawn_resolver(
    ctx: Context,
    token: String,
    streamlink: String,
    channel: String,
    quality: String,
) -> mpsc::Receiver<Resolved> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| {
            let streamlink = match player::resolve_tool(
                &streamlink,
                "streamlink",
                "instala Streamlink o configura su ruta",
            ) {
                Ok(path) => path,
                Err(err) => return Resolved::Err(format!("{err:#}")),
            };
            let source = format!("twitch.tv/{channel}");
            let mut qualities = vec![quality.clone()];
            for fallback in ["best", "720p60", "720p", "480p", "360p", "worst"] {
                if !qualities.iter().any(|value| value == fallback) {
                    qualities.push(fallback.to_string());
                }
            }
            let refs: Vec<&str> = qualities.iter().map(|value| value.as_str()).collect();
            match player::get_hls_url(&streamlink, &token, &source, &refs) {
                Ok((url, _)) => Resolved::Url(url),
                Err(err) => {
                    let text = format!("{err:#}");
                    let lower = text.to_lowercase();
                    if lower.contains("offline")
                        || lower.contains("no playable streams")
                        || lower.contains("not currently live")
                        || lower.contains("404")
                    {
                        Resolved::Offline
                    } else {
                        Resolved::Err(text)
                    }
                }
            }
        })();
        let _ = tx.send(result);
        ctx.request_repaint();
    });
    rx
}
