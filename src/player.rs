use crate::config::{self, Config, Player};
use anyhow::{bail, Context, Result};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Instant;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct RunningStream {
    pub child: Child,
    pub log_path: PathBuf,
    pub started: Instant,
}

impl Drop for RunningStream {
    fn drop(&mut self) {
        // Red de seguridad: si la app se cierra mientras el reproductor sigue
        // vivo, no dejar el proceso huérfano.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Posición y tamaño de la ventana del reproductor, en píxeles físicos.
#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

pub fn launch_at(
    cfg: &Config,
    channel: &str,
    quality: &str,
    audio_only: bool,
    token: &str,
    geometry: Option<Geometry>,
) -> Result<RunningStream> {
    let streamlink = resolve_tool(&cfg.streamlink, "streamlink", "instala Streamlink o configura su ruta")?;
    let source = format!("twitch.tv/{channel}");

    let (hls_url, _got_quality) = if audio_only {
        get_hls_url(&streamlink, token, &source, &["audio_only", "worst"])
            .context("no se pudo obtener la URL de audio")?
    } else {
        let mut qualities = vec![quality.to_string()];
        for q in ["720p60", "720p", "480p", "360p", "worst"] {
            if !qualities.iter().any(|x| x == q) {
                qualities.push(q.to_string());
            }
        }
        let refs: Vec<&str> = qualities.iter().map(|s| s.as_str()).collect();
        get_hls_url(&streamlink, token, &source, &refs)
            .context("no se pudo obtener la URL del stream")?
    };

    let log_path = config::log_path(channel);
    let log_out = File::create(&log_path)
        .with_context(|| format!("no se pudo crear {}", log_path.display()))?;
    let log_err = log_out.try_clone()?;

    let mut cmd = match cfg.player {
        Player::Vlc => {
            let vlc = resolve_tool(&cfg.vlc, "vlc", "instala VLC o configura su ruta")?;
            let mut c = Command::new(vlc);
            c.arg("--loop")
                .arg(format!("--meta-title={channel}"))
                .arg("--qt-minimal-view")
                .arg("--no-video-title-show")
                .arg("--no-one-instance")
                .arg("--no-qt-privacy-ask")
                .arg("--no-qt-error-dialogs")
                .arg("--no-media-library")
                .arg("--quiet");
            if audio_only {
                c.arg("--no-video");
            }
            if let Some(g) = geometry {
                c.arg(format!("--video-x={}", g.x))
                    .arg(format!("--video-y={}", g.y))
                    .arg(format!("--width={}", g.width))
                    .arg(format!("--height={}", g.height));
            }
            c.arg(&hls_url);
            c
        }
        Player::Mpv => {
            let mpv = resolve_tool(&cfg.mpv, "mpv", "instala mpv o configura su ruta")?;
            let mut c = Command::new(mpv);
            c.arg("--loop-file=inf")
                .arg("--no-config")
                .arg(format!("--title={channel}"))
                .arg("--force-window=yes")
                .arg("--keep-open=no")
                .arg("--volume=70")
                .arg("--audio-exclusive=no")
                .arg("--cache=yes");
            if audio_only {
                c.arg("--no-video").arg("--volume=100");
            }
            if cfg.hwaccel {
                c.arg("--hwdec=auto-safe");
            }
            if let Some(g) = geometry {
                c.arg(format!(
                    "--geometry={}x{}{:+}{:+}",
                    g.width, g.height, g.x, g.y
                ));
            }
            c.arg(&hls_url);
            c
        }
    };

    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(log_out))
        .stderr(Stdio::from(log_err));

    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let child = cmd
        .spawn()
        .with_context(|| format!("no se pudo iniciar el reproductor para {channel}"))?;

    Ok(RunningStream {
        child,
        log_path,
        started: Instant::now(),
    })
}

pub(crate) fn get_hls_url(
    streamlink: &Path,
    token: &str,
    source: &str,
    qualities: &[&str],
) -> Result<(String, String)> {
    let mut last_error = String::new();
    for quality in qualities {
        let mut cmd = Command::new(streamlink);
        cmd.arg("--stream-url");
        if !token.is_empty() {
            cmd.arg("--twitch-api-header")
                .arg(format!("Authorization={token}"));
        }
        cmd.arg(source).arg(quality);
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let output = cmd.output().context("no se pudo ejecutar Streamlink")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Some(url) = stdout.lines().rev().find(|l| l.trim().starts_with("http")) {
            return Ok((url.trim().to_string(), (*quality).to_string()));
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        last_error = stderr.lines().last().unwrap_or("sin salida").to_string();
    }
    bail!("Streamlink no devolvió una URL ({last_error})")
}

pub fn stop(stream: &mut RunningStream) {
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &stream.child.id().to_string()])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
    }
    let _ = stream.child.kill();
    let _ = stream.child.wait();
}

pub fn resolve_tool(configured: &str, fallback: &str, hint: &str) -> Result<PathBuf> {
    let configured = configured.trim();
    if !configured.is_empty() {
        let path = PathBuf::from(configured);
        if path.is_absolute() {
            if path.exists() {
                return Ok(fix_vlc_wrapper(path));
            }
            bail!("no existe '{}': {hint}", path.display());
        }
        if let Ok(found) = which::which(configured) {
            return Ok(fix_vlc_wrapper(found));
        }
    }
    if let Ok(found) = which::which(fallback) {
        return Ok(fix_vlc_wrapper(found));
    }
    if let Some(found) = common_location(fallback) {
        return Ok(fix_vlc_wrapper(found));
    }
    bail!("no se encontró '{fallback}': {hint}")
}

fn fix_vlc_wrapper(path: PathBuf) -> PathBuf {
    if path
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.eq_ignore_ascii_case("vlc.com"))
        .unwrap_or(false)
    {
        let exe = path.with_file_name("vlc.exe");
        if exe.exists() {
            return exe;
        }
    }
    path
}

fn common_location(tool: &str) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    let cwd = std::env::current_dir().ok();

    match tool {
        "vlc" => {
            candidates.push(PathBuf::from(r"C:\Program Files\VideoLAN\VLC\vlc.exe"));
            candidates.push(PathBuf::from(r"C:\Program Files (x86)\VideoLAN\VLC\vlc.exe"));
        }
        "mpv" => {
            candidates.push(PathBuf::from(r"C:\Program Files\mpv\mpv.exe"));
            if let Ok(local) = std::env::var("LOCALAPPDATA") {
                candidates.push(PathBuf::from(&local).join(r"Programs\mpv\mpv.exe"));
            }
        }
        "streamlink" => {
            if let Some(cwd) = &cwd {
                candidates.push(cwd.join(r".venv\Scripts\streamlink.exe"));
                if let Some(parent) = cwd.parent() {
                    candidates.push(parent.join(r".venv\Scripts\streamlink.exe"));
                }
            }
            if let Ok(local) = std::env::var("LOCALAPPDATA") {
                candidates.push(PathBuf::from(&local).join(r"Programs\Streamlink\streamlink.exe"));
            }
        }
        "ffmpeg" => {
            if let Ok(local) = std::env::var("LOCALAPPDATA") {
                candidates.push(PathBuf::from(&local).join(r"Microsoft\WinGet\Links\ffmpeg.exe"));
            }
        }
        _ => {}
    }

    candidates.into_iter().find(|p| p.exists())
}

pub fn read_tail(path: &Path, lines: usize) -> String {
    use std::io::{Read, Seek, SeekFrom};

    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    // No cargar un log enorme en memoria: leer como máximo los últimos 256 KB.
    let start = len.saturating_sub(256 * 1024);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    all[start..].join("\n")
}
