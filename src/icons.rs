//! Iconos usando iconflow (fuentes tipográficas gratuitas de Lucide).
//!
//! Renderiza iconos como texto con la fuente de iconflow en lugar de dibujarlos
//! a código con egui::Painter. Esto da iconos más bonitos y nítidos.

use crate::theme::Palette;
use eframe::egui::{
    self, Color32, FontId, FontFamily, Pos2, Rect, Response, Sense, Stroke, StrokeKind,
};
use iconflow::{try_icon, Pack, Size, Style};

// ---------------------------------------------------------------------------
// Iconos disponibles (mismos nombres que antes, ahora resueltos desde Lucide)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Icon {
    Play,
    Pause,
    Stop,
    Heart,
    Grid,
    List,
    Queue,
    Volume,
    Mute,
    Headphones,
    Trash,
    Logs,
    Refresh,
    Moon,
    Sun,
    Search,
    Plus,
    Link,
    ChevronDown,
    Settings,
    Users,
    Gamepad,
    Tv,
    Check,
    Star,
    Gift,
    EyeBolt,
    External,
    Video,
    Chat,
    Gem,
    Bolt,
    Eye,
    Inventory,
    Smile,
    SevenTv,
    ThumbsUp,
    ThumbsDown,
    Clock,
    Close,
    ChevronLeft,
    ChevronRight,
}

// ---------------------------------------------------------------------------
// Mapeo a nombres de Lucide
// ---------------------------------------------------------------------------

fn lucide_name(icon: Icon) -> &'static str {
    match icon {
        Icon::Play => "play",
        Icon::Pause => "pause",
        Icon::Stop => "circle-stop",
        Icon::Heart => "heart",
        Icon::Grid => "layout-grid",
        Icon::List => "list",
        Icon::Queue => "list",
        Icon::Volume => "volume-2",
        Icon::Mute => "volume-x",
        Icon::Headphones => "headphones",
        Icon::Trash => "trash-2",
        Icon::Logs => "file-text",
        Icon::Refresh => "rotate-cw",
        Icon::Moon => "moon",
        Icon::Sun => "sun",
        Icon::Search => "search",
        Icon::Plus => "plus",
        Icon::Link => "link",
        Icon::ChevronDown => "chevron-down",
        Icon::Settings => "settings",
        Icon::Users => "users",
        Icon::Gamepad => "gamepad-2",
        Icon::Tv => "tv",
        Icon::Check => "check",
        Icon::Star => "star",
        Icon::Gift => "gift",
        Icon::EyeBolt => "eye",
        Icon::External => "external-link",
        Icon::Video => "video",
        Icon::Chat => "message-circle",
        Icon::Gem => "gem",
        Icon::Bolt => "zap",
        Icon::Eye => "eye",
        Icon::Inventory => "package",
        Icon::Smile => "smile",
        Icon::SevenTv => "tv",
        Icon::ThumbsUp => "thumbs-up",
        Icon::ThumbsDown => "thumbs-down",
        Icon::Clock => "clock",
        Icon::Close => "x",
        Icon::ChevronLeft => "chevron-left",
        Icon::ChevronRight => "chevron-right",
    }
}

/// Resuelve un icono de Lucide a su carácter Unicode y familia de fuente.
fn resolve_icon(icon: Icon) -> Option<iconflow::IconRef> {
    try_icon(Pack::Lucide, lucide_name(icon), Style::Regular, Size::Regular).ok()
}

/// Pinta un icono dentro de `rect` usando el pintor de egui y la fuente de iconflow.
pub fn paint(
    painter: &egui::Painter,
    rect: Rect,
    icon: Icon,
    color: Color32,
    _bg: Color32,
) {
    let s = rect.width().min(rect.height());
    let centered = Rect::from_center_size(rect.center(), egui::Vec2::splat(s));

    if let Some(r#ref) = resolve_icon(icon) {
        if let Some(ch) = char::from_u32(r#ref.codepoint) {
            let font_id = FontId::new(
                rect.height() * 0.75,
                FontFamily::Name(r#ref.family.into()),
            );
            let galley = painter.layout_no_wrap(ch.to_string(), font_id, color);
            let pos = centered.center() - galley.size() / 2.0;
            painter.galley(pos, galley, color);
        }
    }
}

/// Pinta un icono simple (sin fondo ni borde) para usar en barras laterales, etc.
#[allow(dead_code)]
pub fn paint_icon(
    painter: &egui::Painter,
    rect: Rect,
    icon: Icon,
    color: Color32,
) {
    paint(painter, rect, icon, color, Color32::TRANSPARENT);
}

/// Botón con icono de iconflow.
pub fn button(
    ui: &mut egui::Ui,
    icon: Icon,
    active: bool,
    enabled: bool,
    tooltip: &str,
    p: &Palette,
) -> Response {
    let size = 30.0;
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), sense);
    if ui.is_rect_visible(rect) {
        let hover = enabled && response.hovered();
        let pressed = enabled && response.is_pointer_button_down_on();
        let hover_t = ui
            .ctx()
            .animate_bool_with_time(response.id.with("btn_hov"), hover, 0.14);
        let bounce = crate::anim::ease(hover_t, crate::anim::Easing::BackOut).clamp(0.0, 1.12);
        let scale = if pressed {
            0.92
        } else {
            1.0 + 0.06 * bounce
        };
        let lift = if pressed {
            0.6
        } else {
            crate::anim::hover_lift(ui.ctx(), response.id.with(("lift",)), hover, 1.2)
        };
        let rect = Rect::from_center_size(
            rect.center() + egui::vec2(0.0, lift),
            rect.size() * scale,
        );
        if hover_t > 0.01 && enabled {
            crate::anim::shadow(
                ui.painter(),
                rect,
                8.0,
                [0, 2],
                8,
                Color32::from_black_alpha((34.0 * hover_t) as u8),
            );
        }
        let bg = if !enabled {
            Color32::TRANSPARENT
        } else if active {
            let base = crate::theme::lerp_color(p.accent_weak, p.accent, hover_t * 0.30);
            if pressed {
                crate::theme::lerp_color(base, Color32::BLACK, 0.12)
            } else {
                base
            }
        } else {
            let base = crate::theme::lerp_color(Color32::TRANSPARENT, p.surface3, hover_t * 0.92);
            if pressed {
                crate::theme::lerp_color(base, p.accent_weak, 0.45)
            } else {
                base
            }
        };
        let border = if !enabled {
            Color32::TRANSPARENT
        } else if active {
            p.accent
        } else {
            crate::theme::with_alpha(p.accent, hover_t * 0.42)
        };
        let stroke_width = if active { 1.4 } else { 1.0 };
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(10), bg);
        if border.a() > 0 {
            ui.painter().rect_stroke(
                rect,
                egui::CornerRadius::same(10),
                Stroke::new(stroke_width, border),
                StrokeKind::Inside,
            );
        }
        // Pintar el icono usando iconflow
        let color = if !enabled {
            p.dim
        } else if active {
            p.accent
        } else {
            crate::theme::lerp_color(p.muted, p.text, hover_t)
        };
        paint(ui.painter(), rect.shrink(2.0), icon, color, Color32::TRANSPARENT);
        if enabled {
            response.on_hover_text(tooltip)
        } else {
            response
        }
    } else {
        response
    }
}

/// Botón ancho con texto e icono (para "Reproducir cola", "Cuadrícula", etc.).
pub fn wide_button(
    ui: &mut egui::Ui,
    text: &str,
    icon: Icon,
    primary: bool,
    enabled: bool,
    p: &Palette,
) -> Response {
    let height = 32.0;
    let font_id = egui::FontId::new(12.5, egui::FontFamily::Proportional);
    let text_color = if !enabled {
        p.dim
    } else if primary {
        p.on_accent
    } else {
        p.text
    };
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), font_id, text_color);
    let icon_size = 16.0;
    let pad_x = 14.0;
    let gap = 8.0;
    let min_width = (pad_x + icon_size + gap + galley.size().x + pad_x).max(height);
    let total_width = if ui.layout().main_dir().is_vertical() {
        ui.available_width().max(min_width)
    } else {
        min_width
    };

    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(total_width, height), sense);
    if ui.is_rect_visible(rect) {
        let hover = enabled && response.hovered();
        let pressed = enabled && response.is_pointer_button_down_on();
        let hover_t = ui
            .ctx()
            .animate_bool_with_time(response.id.with("wide_hov"), hover, 0.14);
        let lift = if pressed {
            0.5
        } else {
            crate::anim::hover_lift(ui.ctx(), response.id.with("wide_lift"), hover, 1.0)
        };
        let draw_rect = rect.translate(egui::vec2(0.0, lift));

        if hover_t > 0.01 && enabled {
            crate::anim::shadow(
                ui.painter(),
                draw_rect,
                8.0,
                [0, 2],
                8,
                Color32::from_black_alpha((34.0 * hover_t) as u8),
            );
        }

        let bg = if !enabled {
            p.surface2
        } else if primary {
            let hover_bg = crate::theme::lerp_color(p.accent, Color32::WHITE, 0.14);
            let base = crate::theme::lerp_color(p.accent, hover_bg, hover_t);
            if pressed {
                crate::theme::lerp_color(base, Color32::BLACK, 0.12)
            } else {
                base
            }
        } else {
            let base = crate::theme::lerp_color(p.surface2, p.surface3, hover_t);
            if pressed {
                crate::theme::lerp_color(base, p.accent_weak, 0.35)
            } else {
                base
            }
        };
        let border = if !enabled {
            p.border
        } else if primary {
            p.accent
        } else {
            crate::theme::lerp_color(p.border2, p.accent, hover_t * 0.65)
        };

        ui.painter()
            .rect_filled(draw_rect, egui::CornerRadius::same(11), bg);
        ui.painter().rect_stroke(
            draw_rect,
            egui::CornerRadius::same(11),
            Stroke::new(1.0, border),
            StrokeKind::Inside,
        );

        let icon_rect = Rect::from_center_size(
            Pos2::new(draw_rect.min.x + pad_x + icon_size * 0.5, draw_rect.center().y),
            egui::vec2(icon_size, icon_size),
        );
        paint(ui.painter(), icon_rect, icon, text_color, Color32::TRANSPARENT);

        let text_pos = Pos2::new(
            draw_rect.min.x + pad_x + icon_size + gap,
            draw_rect.center().y - galley.size().y * 0.5,
        );
        ui.painter().galley(text_pos, galley, text_color);
    }
    response
}

/// Chip redondeado (para filtros, etiquetas, etc.).
#[allow(dead_code)]
pub fn chip(
    ui: &mut egui::Ui,
    text: &str,
    active: bool,
    p: &Palette,
) -> Response {
    let font_id = egui::FontId::new(10.0, egui::FontFamily::Proportional);
    let color = if active { p.on_accent } else { p.text };
    let bg = if active { p.accent } else { p.surface3 };
    let border = if active { p.accent } else { p.border };
    // Ancho estimado para el área de clic
    let text_width = text.len() as f32 * 7.5 + 16.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(text_width, 24.0), Sense::click());
    if ui.is_rect_visible(rect) {
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(8), bg);
        ui.painter().rect_stroke(
            rect,
            egui::CornerRadius::same(8),
            Stroke::new(1.0, border),
            StrokeKind::Inside,
        );
        let galley = ui
            .painter()
            .layout_no_wrap(text.to_string(), font_id, color);
        ui.painter().galley(
            rect.center() - galley.size() / 2.0,
            galley,
            color,
        );
    }
    response
}

/// Icono pequeño con tooltip (para barras laterales compactas).
#[allow(dead_code)]
pub fn mini_icon(
    ui: &mut egui::Ui,
    icon: Icon,
    tooltip: &str,
    color: Color32,
    _p: &Palette,
) -> Response {
    let size = 16.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), Sense::click());
    if ui.is_rect_visible(rect) {
        paint(ui.painter(), rect, icon, color, Color32::TRANSPARENT);
        response.on_hover_text(tooltip)
    } else {
        response
    }
}

/// Pinta un badge de texto (chip) directamente en un pintor.
/// Usado por llamadas que no tienen acceso a `&mut Ui`.
pub fn chip_paint(
    painter: &egui::Painter,
    pos: Pos2,
    text: &str,
    fg_color: Color32,
    bg_color: Color32,
) {
    let font_id = FontId::new(9.0, FontFamily::Proportional);
    let galley = painter.layout_no_wrap(text.to_string(), font_id, fg_color);
    let size = galley.size();
    let rect = Rect::from_min_max(
        pos,
        pos + egui::vec2(size.x + 12.0, size.y + 8.0),
    );
    let radius = (size.y / 2.0 + 4.0) as u8;
    painter.rect_filled(rect, egui::CornerRadius::same(radius), bg_color);
    painter.rect_stroke(
        rect,
        egui::CornerRadius::same(radius),
        Stroke::new(1.0, bg_color.gamma_multiply(0.7)),
        StrokeKind::Inside,
    );
    painter.galley(
        rect.min + egui::vec2(6.0, 4.0),
        galley,
        fg_color,
    );
}

/// Registra las fuentes de iconflow en las definiciones de fuentes de egui.
/// Llamar una vez durante la inicialización de la app (en `StreamHubApp::new`).
#[allow(dead_code)]
pub fn register_fonts(font_defs: &mut egui::FontDefinitions) {
    for asset in iconflow::fonts() {
        font_defs.font_data.insert(
            asset.family.to_string(),
            egui::FontData::from_static(asset.bytes).into(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_icons_resolve() {
        let all = [
            Icon::Play, Icon::Pause, Icon::Stop, Icon::Heart, Icon::Grid, Icon::List,
            Icon::Queue, Icon::Volume, Icon::Mute, Icon::Headphones, Icon::Trash,
            Icon::Logs, Icon::Refresh, Icon::Moon, Icon::Sun, Icon::Search, Icon::Plus,
            Icon::Link, Icon::ChevronDown, Icon::Settings, Icon::Users, Icon::Gamepad,
            Icon::Tv, Icon::Check, Icon::Star, Icon::Gift, Icon::EyeBolt, Icon::External,
            Icon::Video, Icon::Chat, Icon::Gem, Icon::Bolt, Icon::Eye, Icon::Inventory,
            Icon::Smile, Icon::SevenTv, Icon::ThumbsUp, Icon::ThumbsDown, Icon::Clock,
            Icon::Close, Icon::ChevronLeft, Icon::ChevronRight,
        ];
        for icon in all {
            assert!(
                resolve_icon(icon).is_some(),
                "icon {:?} ('{}') failed to resolve in Lucide pack",
                icon,
                lucide_name(icon)
            );
        }
    }
}
