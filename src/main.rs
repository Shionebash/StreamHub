#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod anim;
mod app;
mod chat;
mod config;
mod credentials;
mod emote_cache;
mod embedded;
mod emotes;
mod gql;
mod icons;
mod images;
mod mining;
mod player;
mod pubsub;
mod seven_tv;
mod sevent;
mod theme;
mod twitch;
mod updater;
mod ws;

use eframe::egui;

fn main() -> eframe::Result {
    let mut cfg = config::Config::load();
    // Permite forzar el backend por entorno (lo usa el respaldo automático).
    let renderer = match std::env::var("SH_RENDERER").ok().as_deref() {
        Some("glow") => config::RenderBackend::Glow,
        Some("wgpu") => config::RenderBackend::Wgpu,
        _ => cfg.renderer,
    };
    let result = run(renderer);
    // Si wgpu no puede inicializarse en este equipo, se guarda glow y se
    // relanza un proceso nuevo (reutilizar el event loop tras un fallo de
    // winit no es fiable).
    if result.is_err() && renderer != config::RenderBackend::Glow {
        eprintln!("No se pudo iniciar el backend gráfico {renderer:?}; usando glow");
        cfg.renderer = config::RenderBackend::Glow;
        let _ = cfg.save();
        if let Ok(exe) = std::env::current_exe() {
            if std::process::Command::new(exe)
                .env("SH_RENDERER", "glow")
                .spawn()
                .is_ok()
            {
                std::process::exit(0);
            }
        }
        return run(config::RenderBackend::Glow);
    }
    result
}

fn run(renderer: config::RenderBackend) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("StreamHub")
            .with_app_id("StreamHub")
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([820.0, 520.0])
            .with_icon(app::load_icon()),
        renderer: match renderer {
            config::RenderBackend::Glow => eframe::Renderer::Glow,
            config::RenderBackend::Wgpu => eframe::Renderer::Wgpu,
        },
        ..Default::default()
    };

    eframe::run_native(
        "StreamHub",
        options,
        Box::new(|cc| Ok(Box::new(app::StreamHubApp::new(cc)))),
    )
}
