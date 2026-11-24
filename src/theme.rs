//! Paletas y aplicación del tema visual (claro/oscuro).
//!
//! Los colores replican los de `StreamHub.html` para que la app nativa se
//! sienta como la versión web.

use eframe::egui::{
    self,
    epaint::{Mesh, Vertex},
    Color32, Pos2,
};
use serde::{Deserialize, Serialize};

/// Paletas disponibles. Replican los cinco temas de `StreamHub.html`
/// (`postknight`, `night`, `forest`, `ocean`, `sakura`).
///
/// Los alias `light`/`dark` permiten leer las configuraciones antiguas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    CozyDark,
    CozyLight,
    #[serde(alias = "light")]
    Postknight,
    #[serde(alias = "dark")]
    Night,
    Forest,
    Ocean,
    Sakura,
}

impl Theme {
    pub const ALL: [Theme; 7] = [
        Theme::CozyDark,
        Theme::CozyLight,
        Theme::Postknight,
        Theme::Night,
        Theme::Forest,
        Theme::Ocean,
        Theme::Sakura,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::CozyDark => "Cozy Café",
            Self::CozyLight => "Cozy Milk",
            Self::Postknight => "Postknight",
            Self::Night => "Night",
            Self::Forest => "Forest",
            Self::Ocean => "Ocean",
            Self::Sakura => "Sakura",
        }
    }

    pub fn is_dark(self) -> bool {
        matches!(self, Self::CozyDark | Self::Night)
    }

    /// Acento por defecto de la paleta.
    pub fn default_accent(self) -> &'static str {
        match self {
            Self::CozyDark => "#ff8fab",
            Self::CozyLight => "#f06a8e",
            Self::Postknight => "#e0553a",
            Self::Night => "#9d78f5",
            Self::Forest => "#2eb86a",
            Self::Ocean => "#1e6fd0",
            Self::Sakura => "#d0407a",
        }
    }
}

/// Colores personalizables de forma independiente sobre cualquier tema base.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomColors {
    pub bg: Option<String>,
    pub surface: Option<String>,
    pub accent: Option<String>,
    pub live: Option<String>,
    pub secondary: Option<String>,
    pub text: Option<String>,
}

impl CustomColors {
    pub fn is_empty(&self) -> bool {
        self.bg.is_none()
            && self.surface.is_none()
            && self.accent.is_none()
            && self.live.is_none()
            && self.secondary.is_none()
            && self.text.is_none()
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub bg: Color32,
    pub surface: Color32,
    pub surface2: Color32,
    pub surface3: Color32,
    pub border: Color32,
    pub border2: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub dim: Color32,
    pub accent: Color32,
    pub accent_weak: Color32,
    pub accent_mid: Color32,
    pub on_accent: Color32,
    pub green: Color32,
    pub red: Color32,
    pub gold: Color32,
    pub blue: Color32,
}

pub fn palette(theme: Theme, accent: Color32) -> Palette {
    let (r, g, b) = (accent.r(), accent.g(), accent.b());
    let on_accent = readable_on(accent);
    let alpha = |value: u8, alpha: u8| Color32::from_rgba_unmultiplied(value, value, value, alpha);
    match theme {
        // Opción A — Cozy Pastel Café (oscuro cálido cacao-lavanda)
        Theme::CozyDark => Palette {
            bg: Color32::from_rgb(26, 22, 32),
            surface: Color32::from_rgb(37, 31, 46),
            surface2: Color32::from_rgb(47, 39, 59),
            surface3: Color32::from_rgb(59, 49, 73),
            border: Color32::from_rgba_unmultiplied(255, 210, 230, 22),
            border2: Color32::from_rgba_unmultiplied(255, 210, 230, 44),
            text: Color32::from_rgb(248, 240, 246),
            muted: Color32::from_rgb(186, 168, 188),
            dim: Color32::from_rgb(130, 114, 138),
            accent,
            accent_weak: Color32::from_rgba_unmultiplied(r, g, b, 34),
            accent_mid: Color32::from_rgba_unmultiplied(r, g, b, 64),
            on_accent,
            green: Color32::from_rgb(124, 232, 196),
            red: Color32::from_rgb(255, 122, 158),
            gold: Color32::from_rgb(255, 224, 138),
            blue: Color32::from_rgb(156, 200, 255),
        },
        // Opción A — Cozy Milk (claro leche tibia y pétalos)
        Theme::CozyLight => Palette {
            bg: Color32::from_rgb(255, 249, 245),
            surface: Color32::WHITE,
            surface2: Color32::from_rgb(252, 239, 244),
            surface3: Color32::from_rgb(245, 226, 235),
            border: Color32::from_rgba_unmultiplied(186, 118, 148, 34),
            border2: Color32::from_rgba_unmultiplied(186, 118, 148, 62),
            text: Color32::from_rgb(54, 37, 48),
            muted: Color32::from_rgb(134, 102, 118),
            dim: Color32::from_rgb(182, 154, 168),
            accent,
            accent_weak: Color32::from_rgba_unmultiplied(r, g, b, 28),
            accent_mid: Color32::from_rgba_unmultiplied(r, g, b, 52),
            on_accent,
            green: Color32::from_rgb(47, 184, 138),
            red: Color32::from_rgb(232, 85, 122),
            gold: Color32::from_rgb(228, 168, 64),
            blue: Color32::from_rgb(92, 148, 236),
        },
        // `:root` de StreamHub.html (postknight, claro)
        Theme::Postknight => Palette {
            bg: Color32::from_rgb(250, 246, 238),
            surface: Color32::WHITE,
            surface2: Color32::from_rgb(244, 240, 230),
            surface3: Color32::from_rgb(237, 232, 220),
            border: Color32::from_rgba_unmultiplied(140, 110, 80, 31),
            border2: Color32::from_rgba_unmultiplied(140, 110, 80, 56),
            text: Color32::from_rgb(42, 34, 24),
            muted: Color32::from_rgb(122, 110, 94),
            dim: Color32::from_rgb(191, 176, 154),
            accent,
            accent_weak: Color32::from_rgba_unmultiplied(r, g, b, 24),
            accent_mid: Color32::from_rgba_unmultiplied(r, g, b, 44),
            on_accent,
            green: Color32::from_rgb(63, 184, 90),
            red: Color32::from_rgb(224, 85, 58),
            gold: Color32::from_rgb(240, 168, 48),
            blue: Color32::from_rgb(58, 138, 240),
        },
        // `[data-theme="night"]`
        Theme::Night => Palette {
            bg: Color32::from_rgb(13, 15, 24),
            surface: Color32::from_rgb(19, 22, 37),
            surface2: Color32::from_rgb(26, 29, 48),
            surface3: Color32::from_rgb(34, 38, 60),
            border: alpha(255, 18),
            border2: alpha(255, 36),
            text: Color32::from_rgb(232, 232, 245),
            muted: Color32::from_rgb(122, 125, 154),
            dim: Color32::from_rgb(58, 61, 88),
            accent,
            accent_weak: Color32::from_rgba_unmultiplied(r, g, b, 22),
            accent_mid: Color32::from_rgba_unmultiplied(r, g, b, 48),
            on_accent,
            green: Color32::from_rgb(74, 222, 123),
            red: Color32::from_rgb(245, 101, 101),
            gold: Color32::from_rgb(240, 192, 64),
            blue: Color32::from_rgb(96, 165, 250),
        },
        // `[data-theme="forest"]`
        Theme::Forest => Palette {
            bg: Color32::from_rgb(240, 245, 238),
            surface: Color32::WHITE,
            surface2: Color32::from_rgb(232, 240, 229),
            surface3: Color32::from_rgb(221, 232, 216),
            border: Color32::from_rgba_unmultiplied(60, 110, 60, 31),
            border2: Color32::from_rgba_unmultiplied(60, 110, 60, 56),
            text: Color32::from_rgb(26, 40, 24),
            muted: Color32::from_rgb(90, 112, 88),
            dim: Color32::from_rgb(152, 176, 150),
            accent,
            accent_weak: Color32::from_rgba_unmultiplied(r, g, b, 24),
            accent_mid: Color32::from_rgba_unmultiplied(r, g, b, 44),
            on_accent,
            green: Color32::from_rgb(46, 184, 106),
            red: Color32::from_rgb(224, 85, 58),
            gold: Color32::from_rgb(212, 160, 32),
            blue: Color32::from_rgb(58, 138, 240),
        },
        // `[data-theme="ocean"]`
        Theme::Ocean => Palette {
            bg: Color32::from_rgb(238, 244, 251),
            surface: Color32::WHITE,
            surface2: Color32::from_rgb(228, 238, 248),
            surface3: Color32::from_rgb(216, 232, 244),
            border: Color32::from_rgba_unmultiplied(40, 90, 150, 31),
            border2: Color32::from_rgba_unmultiplied(40, 90, 150, 56),
            text: Color32::from_rgb(14, 30, 53),
            muted: Color32::from_rgb(74, 104, 136),
            dim: Color32::from_rgb(154, 184, 216),
            accent,
            accent_weak: Color32::from_rgba_unmultiplied(r, g, b, 24),
            accent_mid: Color32::from_rgba_unmultiplied(r, g, b, 44),
            on_accent,
            green: Color32::from_rgb(42, 184, 122),
            red: Color32::from_rgb(224, 85, 58),
            gold: Color32::from_rgb(224, 152, 32),
            blue: Color32::from_rgb(30, 111, 208),
        },
        // `[data-theme="sakura"]`
        Theme::Sakura => Palette {
            bg: Color32::from_rgb(253, 245, 247),
            surface: Color32::WHITE,
            surface2: Color32::from_rgb(248, 238, 242),
            surface3: Color32::from_rgb(240, 224, 232),
            border: Color32::from_rgba_unmultiplied(180, 80, 120, 31),
            border2: Color32::from_rgba_unmultiplied(180, 80, 120, 56),
            text: Color32::from_rgb(42, 21, 32),
            muted: Color32::from_rgb(138, 88, 112),
            dim: Color32::from_rgb(200, 160, 184),
            accent,
            accent_weak: Color32::from_rgba_unmultiplied(r, g, b, 24),
            accent_mid: Color32::from_rgba_unmultiplied(r, g, b, 44),
            on_accent,
            green: Color32::from_rgb(80, 184, 112),
            red: Color32::from_rgb(224, 85, 58),
            gold: Color32::from_rgb(224, 152, 48),
            blue: Color32::from_rgb(72, 120, 224),
        },
    }
}

/// Construye la paleta aplicando personalizaciones independientes de color.
pub fn palette_with_overrides(
    theme: Theme,
    accent: Color32,
    overrides: &CustomColors,
) -> Palette {
    let effective_accent = overrides
        .accent
        .as_deref()
        .and_then(crate::config::parse_hex_color)
        .unwrap_or(accent);
    let mut p = palette(theme, effective_accent);

    if let Some(bg) = overrides.bg.as_deref().and_then(crate::config::parse_hex_color) {
        p.bg = bg;
    }
    if let Some(surface) = overrides
        .surface
        .as_deref()
        .and_then(crate::config::parse_hex_color)
    {
        let dark_surface = is_color_dark(surface);
        let shift = if dark_surface { Color32::WHITE } else { Color32::BLACK };
        p.surface = surface;
        p.surface2 = lerp_color(surface, shift, if dark_surface { 0.06 } else { 0.04 });
        p.surface3 = lerp_color(surface, shift, if dark_surface { 0.13 } else { 0.09 });
        p.border = with_alpha(shift, if dark_surface { 0.10 } else { 0.12 });
        p.border2 = with_alpha(shift, if dark_surface { 0.20 } else { 0.22 });
    }
    if let Some(text) = overrides
        .text
        .as_deref()
        .and_then(crate::config::parse_hex_color)
    {
        p.text = text;
        p.muted = lerp_color(text, p.bg, 0.34);
        p.dim = lerp_color(text, p.bg, 0.56);
    }
    if let Some(live) = overrides
        .live
        .as_deref()
        .and_then(crate::config::parse_hex_color)
    {
        p.red = live;
    }
    if let Some(secondary) = overrides
        .secondary
        .as_deref()
        .and_then(crate::config::parse_hex_color)
    {
        p.green = secondary;
    }
    p
}

pub fn is_color_dark(c: Color32) -> bool {
    let luma = (0.2126 * c.r() as f32 + 0.7152 * c.g() as f32 + 0.0722 * c.b() as f32) / 255.0;
    luma < 0.45
}

/// Color inferior del degradado de fondo. Tinta la base con el acento para
/// que el fondo plano de la app tenga la misma profundidad que la referencia.
pub fn gradient_bottom(p: &Palette) -> Color32 {
    let luminance =
        (0.2126 * p.bg.r() as f32 + 0.7152 * p.bg.g() as f32 + 0.0722 * p.bg.b() as f32) / 255.0;
    let mix = if luminance < 0.4 { 0.13 } else { 0.09 };
    lerp_color(p.bg, p.accent, mix)
}

/// Fondo de la referencia: degradado vertical dibujado en la capa de fondo,
/// detrás de todos los paneles.
pub fn paint_background(ctx: &egui::Context, p: &Palette) {
    let bottom = gradient_bottom(p);
    if bottom == p.bg {
        return;
    }
    let painter = ctx.layer_painter(egui::LayerId::background());
    quad(
        &painter,
        ctx.viewport_rect(),
        p.bg,
        p.bg,
        bottom,
        bottom,
    );
}

/// Rectángulo con un color por esquina (malla de dos triángulos).
pub fn quad(
    painter: &egui::Painter,
    rect: egui::Rect,
    tl: Color32,
    tr: Color32,
    br: Color32,
    bl: Color32,
) {
    let mut mesh = Mesh::default();
    let uv = Pos2::ZERO;
    mesh.vertices.push(Vertex {
        pos: rect.left_top(),
        uv,
        color: tl,
    });
    mesh.vertices.push(Vertex {
        pos: rect.right_top(),
        uv,
        color: tr,
    });
    mesh.vertices.push(Vertex {
        pos: rect.right_bottom(),
        uv,
        color: br,
    });
    mesh.vertices.push(Vertex {
        pos: rect.left_bottom(),
        uv,
        color: bl,
    });
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    painter.add(mesh);
}

/// Fondo del panel de chat, ligeramente distinto de las tarjetas.
pub fn chat_bg(p: &Palette) -> Color32 {
    lerp_color(p.surface, p.bg, 0.32)
}

/// Texto legible sobre un color de acento (luminancia WCAG, como la web).
pub fn readable_on(color: Color32) -> Color32 {
    let luminance =
        (0.2126 * color.r() as f32 + 0.7152 * color.g() as f32 + 0.0722 * color.b() as f32) / 255.0;
    if luminance > 0.58 {
        Color32::from_rgb(17, 17, 20)
    } else {
        Color32::WHITE
    }
}

/// Copia de un color con la opacidad ajustada (para pulsos y fades).
pub fn with_alpha(color: Color32, alpha: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(
        color.r(),
        color.g(),
        color.b(),
        (color.a() as f32 * alpha.clamp(0.0, 1.0)).round() as u8,
    )
}

#[allow(dead_code)]
pub fn apply(ctx: &egui::Context, theme: Theme, accent: Color32) {
    apply_with_overrides(ctx, theme, accent, &CustomColors::default());
}

pub fn apply_with_overrides(
    ctx: &egui::Context,
    theme: Theme,
    accent: Color32,
    overrides: &CustomColors,
) {
    let p = palette_with_overrides(theme, accent, overrides);
    let is_dark = is_color_dark(p.bg);

    let mut visuals = if is_dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };

    visuals.panel_fill = p.bg;
    visuals.window_fill = p.surface;
    visuals.extreme_bg_color = p.surface2;
    visuals.faint_bg_color = p.surface2;
    visuals.code_bg_color = p.surface3;
    visuals.override_text_color = Some(p.text);
    visuals.hyperlink_color = p.accent;
    visuals.selection.bg_fill = p.accent_mid;
    visuals.selection.stroke = egui::Stroke::new(1.0, p.accent);
    visuals.window_stroke = egui::Stroke::new(1.0, p.border2);
    visuals.window_corner_radius = egui::CornerRadius::same(14);
    visuals.menu_corner_radius = egui::CornerRadius::same(12);

    visuals.widgets.noninteractive.bg_fill = p.surface2;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, p.border);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, p.text);
    visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::same(10);

    visuals.widgets.inactive.bg_fill = p.surface2;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, p.border);
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, p.muted);
    visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(10);

    visuals.widgets.hovered.bg_fill = p.surface3;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, p.border2);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, p.text);
    visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(10);

    visuals.widgets.active.bg_fill = p.accent_mid;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, p.accent);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, p.text);
    visuals.widgets.active.corner_radius = egui::CornerRadius::same(10);

    visuals.widgets.open.bg_fill = p.surface3;
    visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0, p.border2);
    visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0, p.text);
    visuals.widgets.open.corner_radius = egui::CornerRadius::same(10);

    ctx.set_theme(if is_dark {
        egui::ThemePreference::Dark
    } else {
        egui::ThemePreference::Light
    });
    ctx.set_visuals_of(egui::Theme::Dark, visuals.clone());
    ctx.set_visuals_of(egui::Theme::Light, visuals);

    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
        style.spacing.interact_size = egui::vec2(30.0, 28.0);
        style.text_styles.insert(
            egui::TextStyle::Heading,
            egui::FontId::new(21.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Body,
            egui::FontId::new(13.5, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Button,
            egui::FontId::new(12.5, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Small,
            egui::FontId::new(11.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Monospace,
            egui::FontId::new(10.5, egui::FontFamily::Monospace),
        );
    });
}

/// Carga una fuente CJK del sistema como respaldo para nombres y títulos en
/// japonés/chino/coreano. Se usa una sola fuente para no disparar la memoria.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let candidates = [
        "C:/Windows/Fonts/YuGothR.ttc", // japonés
        "C:/Windows/Fonts/msyh.ttc",    // chino (incluye kana)
        "C:/Windows/Fonts/malgun.ttf",  // coreano
    ];
    for (index, path) in candidates.iter().enumerate() {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let name = format!("system-cjk-{index}");
        fonts.font_data.insert(
            name.clone(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes)),
        );
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push(name.clone());
        }
        ctx.set_fonts(fonts);
        return;
    }
}

/// Interpola dos colores RGBA (para animaciones de hover).
pub fn lerp_color(from: Color32, to: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(
        mix(from.r(), to.r()),
        mix(from.g(), to.g()),
        mix(from.b(), to.b()),
        mix(from.a(), to.a()),
    )
}

/// Color estable a partir del nombre del canal (para avatares).
pub fn name_color(name: &str, theme: Theme) -> Color32 {
    let mut hash: u32 = 2166136261;
    for byte in name.bytes() {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(16777619);
    }
    let hue = (hash % 360) as f32;
    let (sat, val) = if theme.is_dark() {
        (0.62, 0.82)
    } else {
        (0.68, 0.78)
    };
    hsv_to_rgb(hue, sat, val)
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> Color32 {
    let c = v * s;
    let hp = (h / 60.0) % 6.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    Color32::from_rgb(
        ((r + m) * 255.0).round() as u8,
        ((g + m) * 255.0).round() as u8,
        ((b + m) * 255.0).round() as u8,
    )
}
