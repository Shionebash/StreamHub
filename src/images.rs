//! Caché de miniaturas (previews de Twitch, emotes de 7TV y portadas).
//!
//! Las descargas se hacen en hilos cortos y el resultado se convierte en una
//! textura de egui en el hilo de UI. Nunca se bloquea el repintado y hay un
//! límite de descargas simultáneas para no disparar el número de hilos.
//!
//! Los emotes animados (GIF/WebP) se decodifican fotograma a fotograma y se
//! animan en la propia textura. Primero se muestra el primer fotograma en
//! cuanto se descarga y, después, se decodifica la animación completa en
//! segundo plano.

use eframe::egui::{self, ColorImage, ImageData, TextureHandle, TextureOptions};
use image::codecs::gif::GifDecoder;
use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder, ImageFormat};
use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Texturas estáticas vivas. Cada una a 32×32 ocupa solo 4 KB, así que un
/// límite amplio evita el "thrashing" cuando el selector muestra muchas
/// celdas y las texturas se expulsan antes de volver a dibujarse.
const MAX_TEXTURES: usize = 640;
/// Animaciones en memoria. Cada una guarda sus fotogramas ya reducidos.
const MAX_ANIMATED: usize = 60;
/// Fotogramas máximos por animación (se reparten si el GIF tiene más).
const MAX_FRAMES: usize = 24;
/// Descargas simultáneas máximas en cola.
const MAX_PENDING: usize = 12;
/// Hilos fijos del pool de descarga y decodificación de imágenes.
const WORKER_THREADS: usize = 4;
/// Un GIF real nunca va tan rápido; evita repintados absurdos.
const MIN_FRAME_MS: u32 = 20;
/// Los emotes son pequeños: animarlos a 60 fps solo gasta CPU. Se repinta
/// como máximo a esta cadencia; los fotogramas que toquen se calculan igual
/// por tiempo, así que la animación no se acelera ni se ralentiza.
const MAX_ANIMATION_FPS: u32 = 15;
/// Los GIF enormes pueden tardar minutos en decodificarse en debug y dejan
/// al resto esperando. Por encima de este tamaño se queda el primer fotograma.
const MAX_ANIMATION_BYTES: usize = 2_500_000;
/// Tope de fotogramas que se decodifican de un archivo (luego se reparten).
const MAX_DECODE_FRAMES: usize = 160;
/// Si una imagen no se puede descargar o decodificar, no se reintenta hasta
/// pasado este tiempo. Sin esto, cada frame volvía a lanzar la descarga.
const FAIL_RETRY: Duration = Duration::from_secs(10 * 60);
/// Máximo de texturas nuevas subidas a la GPU por frame para evitar tirones
/// al abrir grillas con muchas miniaturas o el selector de emotes.
const MAX_UPLOADS_PER_FRAME: usize = 4;

pub struct ImageCache {
    textures: HashMap<String, TextureHandle>,
    animated: HashMap<String, AnimatedTexture>,
    /// Descargas en curso: URL → época en la que se lanzaron.
    pending: HashMap<String, u64>,
    /// URL → instante del último fallo, para no reintentar en bucle.
    failed: HashMap<String, Instant>,
    order: VecDeque<String>,
    clock: u64,
    /// Contador de frames e instante cacheados una vez por frame en `poll()`.
    frame_id: u64,
    frame_now: f64,
    /// Buffer reutilizable para construir claves de caché sin alocar en hits.
    key_buf: String,
    /// Sube al purgar: los mensajes de descargas antiguas se ignoran.
    epoch: u64,
    tx: Sender<(u64, String, LoadMessage)>,
    rx: Receiver<(u64, String, LoadMessage)>,
    jobs_tx: Sender<FetchJob>,
}

struct FetchJob {
    epoch: u64,
    key: String,
    url: String,
    width: u32,
    height: u32,
    file: Option<PathBuf>,
    tx: Sender<(u64, String, LoadMessage)>,
    ctx: egui::Context,
}

enum LoadMessage {
    /// Primer fotograma: aparece al instante mientras se decodifica el resto.
    Preview(Option<ColorImage>),
    /// Resultado final. `None` deja el preview (si lo hubo) tal cual.
    Done(Option<LoadedImage>),
    /// No se pudo descargar ni decodificar.
    Failed,
}

enum LoadedImage {
    Static(ColorImage),
    Animated(Vec<AnimatedFrame>),
}

struct AnimatedFrame {
    image: Arc<ColorImage>,
    delay_ms: u32,
}

struct AnimatedTexture {
    frames: Vec<AnimatedFrame>,
    total_ms: u32,
    texture: TextureHandle,
    frame: usize,
    started: f64,
    last_used: u64,
    last_frame_id: u64,
}

impl Default for ImageCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ImageCache {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let (jobs_tx, jobs_rx) = mpsc::channel::<FetchJob>();
        let shared_rx = Arc::new(Mutex::new(jobs_rx));
        for index in 0..WORKER_THREADS {
            let worker_rx = Arc::clone(&shared_rx);
            let _ = std::thread::Builder::new()
                .name(format!("img-worker-{index}"))
                .spawn(move || loop {
                    let job = {
                        let Ok(guard) = worker_rx.lock() else {
                            break;
                        };
                        guard.recv()
                    };
                    match job {
                        Ok(job) => fetch_and_send(job),
                        Err(_) => break,
                    }
                });
        }
        Self {
            textures: HashMap::new(),
            animated: HashMap::new(),
            pending: HashMap::new(),
            failed: HashMap::new(),
            order: VecDeque::new(),
            clock: 0,
            frame_id: 0,
            frame_now: 0.0,
            key_buf: String::with_capacity(128),
            epoch: 0,
            tx,
            rx,
            jobs_tx,
        }
    }

    /// Procesa las texturas descargadas. Llamar una vez por frame.
    pub fn poll(&mut self, ctx: &egui::Context) {
        self.frame_id = self.frame_id.wrapping_add(1);
        self.frame_now = ctx.input(|input| input.time);

        let mut received = false;
        let mut uploads = 0usize;
        while uploads < MAX_UPLOADS_PER_FRAME {
            let Ok((epoch, key, message)) = self.rx.try_recv() else {
                break;
            };
            if epoch != self.epoch {
                // Descarga lanzada antes de cerrar el chat. Se descarta y se
                // libera su hueco (solo si sigue siendo de esa misma época).
                if self.pending.get(&key) == Some(&epoch) {
                    self.pending.remove(&key);
                }
                continue;
            }
            received = true;
            match message {
                LoadMessage::Preview(image) => {
                    // El preview no bloquea la descarga: se libera el hueco
                    // para que sigan cargando los demás emotes mientras este
                    // termina de decodificar su animación.
                    self.pending.remove(&key);
                    if let Some(image) = image {
                        self.insert_static(ctx, key, image);
                        uploads += 1;
                    }
                }
                LoadMessage::Done(loaded) => {
                    self.pending.remove(&key);
                    self.failed.remove(&key);
                    match loaded {
                        Some(LoadedImage::Static(image)) => {
                            self.insert_static(ctx, key, image);
                            uploads += 1;
                        }
                        Some(LoadedImage::Animated(frames)) => {
                            self.insert_animated(ctx, key, frames);
                            uploads += 1;
                        }
                        None => {}
                    }
                }
                LoadMessage::Failed => {
                    self.pending.remove(&key);
                    self.failed.insert(key, Instant::now());
                }
            }
        }
        if received {
            ctx.request_repaint();
        }
    }

    fn insert_static(&mut self, ctx: &egui::Context, key: String, image: ColorImage) {
        let texture = ctx.load_texture(&key, image, TextureOptions::LINEAR);
        // Si había una animación con la misma clave, el primer fotograma la
        // sustituye (por ejemplo al volver a pedir una imagen tras limpiar).
        self.animated.remove(&key);
        if self.textures.insert(key.clone(), texture).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > MAX_TEXTURES {
            if let Some(old) = self.order.pop_front() {
                self.textures.remove(&old);
            }
        }
    }

    fn insert_animated(&mut self, ctx: &egui::Context, key: String, frames: Vec<AnimatedFrame>) {
        if frames.is_empty() {
            return;
        }
        // La animación ocupa el sitio del preview estático de la misma URL.
        self.textures.remove(&key);
        self.order.retain(|old| old != &key);
        let first = ImageData::Color(Arc::clone(&frames[0].image));
        let texture = ctx.load_texture(&key, first, TextureOptions::LINEAR);
        let total_ms = frames
            .iter()
            .map(|frame| frame.delay_ms)
            .sum::<u32>()
            .max(1);
        self.animated.insert(
            key,
            AnimatedTexture {
                frames,
                total_ms,
                texture,
                frame: 0,
                started: self.frame_now,
                last_used: self.clock,
                last_frame_id: self.frame_id,
            },
        );
        self.trim_animated();
    }

    fn trim_animated(&mut self) {
        while self.animated.len() > MAX_ANIMATED {
            let Some(old) = self
                .animated
                .iter()
                .min_by_key(|(_, anim)| anim.last_used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            // Al salir del presupuesto se conserva el fotograma actual como
            // imagen fija: no hay que volver a descargar nada.
            if let Some(mut anim) = self.animated.remove(&old) {
                anim.texture
                    .set(anim.frames[anim.frame].image.clone(), TextureOptions::LINEAR);
                if self.textures.insert(old.clone(), anim.texture).is_none() {
                    self.order.push_back(old);
                }
                while self.order.len() > MAX_TEXTURES {
                    if let Some(evicted) = self.order.pop_front() {
                        self.textures.remove(&evicted);
                    }
                }
            }
        }
    }

    /// Devuelve la textura si ya está lista; si no, lanza la descarga.
    pub fn get(
        &mut self,
        ctx: &egui::Context,
        url: &str,
        width: u32,
        height: u32,
    ) -> Option<TextureHandle> {
        self.get_cached(ctx, url, width, height, None)
    }

    /// Igual que [`Self::get`], pero usando una carpeta de caché en disco.
    ///
    /// Si la imagen ya está descargada se lee del disco (sin red). Si no, se
    /// descarga y se guarda para la próxima vez. Las descargas de los emotes
    /// de un canal viven en `emotes/<canal>/img/`.
    pub fn get_cached(
        &mut self,
        ctx: &egui::Context,
        url: &str,
        width: u32,
        height: u32,
        cache_dir: Option<&std::path::Path>,
    ) -> Option<TextureHandle> {
        // El poll se hace una vez por frame en `StreamHubApp::ui`; hacerlo
        // aquí multiplicaba por cada emote el bloqueo del canal de mensajes.
        let base_len = write_cache_key(&mut self.key_buf, url, width, height);
        if base_len == 0 {
            return None;
        }

        if let Some(texture) = self.textures.get(self.key_buf.as_str()) {
            return Some(texture.clone());
        }

        if let Some(anim) = self.animated.get_mut(self.key_buf.as_str()) {
            if anim.last_frame_id == self.frame_id {
                return Some(anim.texture.clone());
            }
            self.clock += 1;
            anim.last_used = self.clock;
            anim.last_frame_id = self.frame_id;
            let elapsed_ms =
                (((self.frame_now - anim.started).max(0.0)) * 1000.0) as u32 % anim.total_ms;
            let (index, remaining) = anim.frame_at(elapsed_ms);
            if index != anim.frame {
                anim.texture.set(
                    ImageData::Color(Arc::clone(&anim.frames[index].image)),
                    TextureOptions::LINEAR,
                );
                anim.frame = index;
            }
            // Cada emote pide el siguiente repintado; egui usa el más cercano.
            // El mínimo de 15 fps evita repintar toda la interfaz decenas de
            // veces por segundo solo por los emotes.
            let wait = remaining
                .max(1_000 / MAX_ANIMATION_FPS)
                .max(MIN_FRAME_MS);
            ctx.request_repaint_after(Duration::from_millis(u64::from(wait)));
            return Some(anim.texture.clone());
        }

        if self.pending.contains_key(self.key_buf.as_str()) {
            return None;
        }
        if let Some(at) = self.failed.get(self.key_buf.as_str()) {
            if at.elapsed() < FAIL_RETRY {
                return None;
            }
            self.failed.remove(self.key_buf.as_str());
        }
        // No se lanza más de `MAX_PENDING` descargas a la vez.
        if self.pending.len() >= MAX_PENDING {
            return None;
        }
        let epoch = self.epoch;
        let key = self.key_buf.clone();
        let base = self.key_buf[..base_len].to_string();
        self.pending.insert(key.clone(), epoch);
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let file = cache_dir.map(|dir| cache_file(dir, &base));
        let _ = self.jobs_tx.send(FetchJob {
            epoch,
            key,
            url: base,
            width,
            height,
            file,
            tx,
            ctx,
        });
        None
    }

    #[allow(dead_code)]
    pub fn preload(&mut self, ctx: &egui::Context, url: &str, width: u32, height: u32) {
        let _ = self.get(ctx, url, width, height);
    }

    /// Libera de la caché las texturas cuya URL contiene `needle`.
    ///
    /// Se usa al cerrar el chat por completo para devolver la memoria de los
    /// emotes descargados.
    pub fn purge(&mut self, needle: &str) {
        self.epoch = self.epoch.wrapping_add(1);
        self.textures.retain(|key, _| !key.contains(needle));
        self.animated.retain(|key, _| !key.contains(needle));
        self.pending.retain(|key, _| !key.contains(needle));
        self.failed.retain(|key, _| !key.contains(needle));
        let order: VecDeque<String> = self
            .order
            .iter()
            .filter(|key| self.textures.contains_key(*key))
            .cloned()
            .collect();
        self.order = order;
    }
}

impl AnimatedTexture {
    /// Fotograma visible y milisegundos que le quedan.
    fn frame_at(&self, elapsed_ms: u32) -> (usize, u32) {
        let mut acc = 0u32;
        for (index, frame) in self.frames.iter().enumerate() {
            acc = acc.saturating_add(frame.delay_ms);
            if elapsed_ms < acc {
                return (index, acc - elapsed_ms);
            }
        }
        // Si algo cuadra mal, vuelve al principio.
        (0, self.frames.first().map_or(MIN_FRAME_MS, |f| f.delay_ms))
    }
}

/// Decodifica y envía la imagen. El primer fotograma de una animación se
/// envía antes de decodificar el resto para que aparezca al instante.
fn fetch_and_send(job: FetchJob) {
    let FetchJob {
        epoch,
        key,
        url,
        width,
        height,
        file,
        tx,
        ctx,
    } = job;
    let (mut bytes, from_disk) = match file.as_deref().and_then(read_file) {
        Some(bytes) => (bytes, true),
        None => match download_bytes(&url) {
            Some(bytes) => {
                if let Some(path) = file.as_deref() {
                    write_file(path, &bytes);
                }
                (bytes, false)
            }
            None => {
                log_failure(&url);
                send(epoch, &tx, &ctx, key, LoadMessage::Failed);
                return;
            }
        },
    };

    let mut preview = decode_static(&bytes, width, height);
    // Archivo corrupto o formato desconocido: se vuelve a descargar.
    if preview.is_none() && from_disk {
        if let Some(fresh) = download_bytes(&url) {
            if let Some(path) = file.as_deref() {
                write_file(path, &fresh);
            }
            preview = decode_static(&fresh, width, height);
            bytes = fresh;
        }
    }

    let format = image::guess_format(&bytes).ok();
    if is_animated_candidate(format, &bytes) && bytes.len() <= MAX_ANIMATION_BYTES {
        if let Some(image) = preview.clone() {
            send(epoch, &tx, &ctx, key.clone(), LoadMessage::Preview(Some(image)));
        }
        let frames = decode_animation(&bytes, format, width, height);
        if frames.len() > 1 {
            send(
                epoch,
                &tx,
                &ctx,
                key,
                LoadMessage::Done(Some(LoadedImage::Animated(frames))),
            );
        } else if preview.is_some() {
            // Ya se enseñó el primer fotograma; no hace falta reemplazarlo.
            send(epoch, &tx, &ctx, key, LoadMessage::Done(None));
        } else if let Some(image) = decode_static(&bytes, width, height) {
            send(
                epoch,
                &tx,
                &ctx,
                key,
                LoadMessage::Done(Some(LoadedImage::Static(image))),
            );
        } else {
            log_failure(&url);
            send(epoch, &tx, &ctx, key, LoadMessage::Failed);
        }
    } else if let Some(image) = preview {
        send(
            epoch,
            &tx,
            &ctx,
            key,
            LoadMessage::Done(Some(LoadedImage::Static(image))),
        );
    } else {
        log_failure(&url);
        send(epoch, &tx, &ctx, key, LoadMessage::Failed);
    }
}

fn log_failure(url: &str) {
    #[cfg(debug_assertions)]
    if std::env::var_os("SH_DEBUG").is_some() {
        eprintln!("[img] no se pudo cargar {url}");
    }
    #[cfg(not(debug_assertions))]
    let _ = url;
}

fn send(
    epoch: u64,
    tx: &Sender<(u64, String, LoadMessage)>,
    ctx: &egui::Context,
    key: String,
    message: LoadMessage,
) {
    let _ = tx.send((epoch, key, message));
    ctx.request_repaint();
}

fn read_file(path: &Path) -> Option<Vec<u8>> {
    let bytes = fs::read(path).ok()?;
    if bytes.is_empty() {
        None
    } else {
        Some(bytes)
    }
}

fn write_file(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, bytes);
}

fn client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .user_agent("StreamHubNative/0.1")
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default()
    })
}

fn download_bytes(url: &str) -> Option<Vec<u8>> {
    let response = client().get(url).send().ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.bytes().ok().map(|bytes| bytes.to_vec())
}

fn decode_static(bytes: &[u8], width: u32, height: u32) -> Option<ColorImage> {
    let image = image::load_from_memory(bytes).ok()?;
    let image = image.resize_to_fill(width, height, image::imageops::FilterType::Triangle);
    let image = image.to_rgba8();
    Some(ColorImage::from_rgba_unmultiplied(
        [width as usize, height as usize],
        image.as_raw(),
    ))
}

fn is_animated_candidate(format: Option<ImageFormat>, bytes: &[u8]) -> bool {
    match format {
        Some(ImageFormat::Gif) => true,
        Some(ImageFormat::WebP) => WebPDecoder::new(Cursor::new(bytes))
            .map(|decoder| decoder.has_animation())
            .unwrap_or(false),
        _ => false,
    }
}

/// Decodifica todos los fotogramas y los reduce al tamaño de dibujo.
///
/// La decodificación de animaciones se serializa: decodificar decenas de GIF
/// a la vez saturaba la CPU y hacía que la interfaz fuera a tirones.
fn decode_animation(bytes: &[u8], format: Option<ImageFormat>, width: u32, height: u32) -> Vec<AnimatedFrame> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let lock = LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock.lock().unwrap_or_else(|poison| poison.into_inner());
    match format {
        Some(ImageFormat::Gif) => GifDecoder::new(Cursor::new(bytes))
            .ok()
            .and_then(|decoder| frames_from(decoder, width, height)),
        Some(ImageFormat::WebP) => WebPDecoder::new(Cursor::new(bytes))
            .ok()
            .filter(|decoder| decoder.has_animation())
            .and_then(|decoder| frames_from(decoder, width, height)),
        _ => None,
    }
    .unwrap_or_default()
}

fn frames_from<'a, D: AnimationDecoder<'a>>(
    decoder: D,
    width: u32,
    height: u32,
) -> Option<Vec<AnimatedFrame>> {
    // Se reduce cada fotograma en cuanto se decodifica: guardar todos los
    // fotogramas a resolución original disparaba la memoria con GIF grandes.
    let mut frames: Vec<AnimatedFrame> = Vec::new();
    for frame in decoder.into_frames().take(MAX_DECODE_FRAMES) {
        let frame = frame.ok()?;
        let (num, den) = frame.delay().numer_denom_ms();
        let delay_ms = ((num as f64) / (den.max(1) as f64))
            .round()
            .clamp(MIN_FRAME_MS as f64, 500.0) as u32;
        let buffer = frame.into_buffer();
        let resized = image::imageops::resize(
            &buffer,
            width,
            height,
            image::imageops::FilterType::Triangle,
        );
        frames.push(AnimatedFrame {
            image: Arc::new(ColorImage::from_rgba_unmultiplied(
                [width as usize, height as usize],
                resized.as_raw(),
            )),
            delay_ms,
        });
    }
    if frames.len() < 2 {
        return None;
    }
    Some(decimate(frames))
}

/// Si hay demasiados fotogramas se conservan `MAX_FRAMES` repartidos y se
/// suma el tiempo de los descartados, para no acelerar la animación.
fn decimate(frames: Vec<AnimatedFrame>) -> Vec<AnimatedFrame> {
    let len = frames.len();
    if len <= MAX_FRAMES {
        return frames;
    }
    let mut out: Vec<AnimatedFrame> = Vec::with_capacity(MAX_FRAMES);
    let mut next_target = 0usize;
    let mut kept = 0usize;
    let mut accumulated = 0u32;
    for (index, frame) in frames.into_iter().enumerate() {
        accumulated = accumulated.saturating_add(frame.delay_ms);
        if index == next_target {
            out.push(AnimatedFrame {
                image: frame.image,
                delay_ms: accumulated.min(1_000),
            });
            accumulated = 0;
            kept += 1;
            next_target = kept * len / MAX_FRAMES;
        }
    }
    out
}

/// Nombre de archivo estable para una URL dentro de la carpeta del canal.
fn cache_file(dir: &Path, url: &str) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hasher);
    let hash = hasher.finish();
    let clean = url.split(['?', '#']).next().unwrap_or(url);
    let ext = clean.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let ext = match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "avif" => ext,
        _ => "img".to_string(),
    };
    dir.join("img").join(format!("{hash:016x}.{ext}"))
}

/// Escribe la clave `<url_normalizada>@{width}x{height}` en `buf` sin alocar
/// en caché hits y devuelve la longitud del prefijo `<url_normalizada>`.
fn write_cache_key(buf: &mut String, url: &str, width: u32, height: u32) -> usize {
    buf.clear();
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return 0;
    }
    if !trimmed.contains('{') {
        buf.push_str(trimmed);
    } else {
        let mut rest = trimmed;
        while let Some(pos) = rest.find('{') {
            buf.push_str(&rest[..pos]);
            let after = &rest[pos..];
            if let Some(tail) = after.strip_prefix("{width}") {
                let _ = write!(buf, "{width}");
                rest = tail;
            } else if let Some(tail) = after.strip_prefix("{height}") {
                let _ = write!(buf, "{height}");
                rest = tail;
            } else {
                buf.push('{');
                rest = &after[1..];
            }
        }
        buf.push_str(rest);
    }
    let base_len = buf.len();
    let _ = write!(buf, "@{width}x{height}");
    base_len
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_gif_frames() {
        use image::{Delay, Frame, Rgba, RgbaImage};

        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            let red = RgbaImage::from_pixel(8, 8, Rgba([255, 0, 0, 255]));
            let blue = RgbaImage::from_pixel(8, 8, Rgba([0, 0, 255, 255]));
            encoder
                .encode_frames(vec![
                    Frame::from_parts(red, 0, 0, Delay::from_numer_denom_ms(100, 1)),
                    Frame::from_parts(blue, 0, 0, Delay::from_numer_denom_ms(100, 1)),
                ])
                .expect("gif");
        }
        let loaded = decode_animation(&bytes, Some(ImageFormat::Gif), 16, 16);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].image.size, [16, 16]);
        assert_eq!(loaded[0].delay_ms, 100);
        assert!(loaded[1].image.pixels[0].r() < 40);
        assert!(loaded[1].image.pixels[0].b() > 200);
    }

    #[test]
    fn static_images_are_not_animated_candidates() {
        let mut bytes = Vec::new();
        {
            let image = image::RgbaImage::from_pixel(4, 4, image::Rgba([1, 2, 3, 255]));
            image
                .write_to(
                    &mut Cursor::new(&mut bytes),
                    image::ImageFormat::Png,
                )
                .expect("png");
        }
        assert!(!is_animated_candidate(
            image::guess_format(&bytes).ok(),
            &bytes
        ));
    }
}
