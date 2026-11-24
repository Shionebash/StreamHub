//! Animaciones y efectos visuales.
//!
//! Equivalente a las transiciones y keyframes de `StreamHub.html`, pero
//! resueltos con el `Painter` de egui: elevación al hover, sombras difusas,
//! rebote del toast, barras de progreso, interruptores y pulsos.

// Algunas utilidades se usarán en las fases de chat y mining.
#![allow(dead_code)]

use crate::theme::{lerp_color, Palette};
use eframe::egui::{self, epaint::Shadow, Color32, CornerRadius, Rect, Response, Sense, Vec2};

/// Curvas de animación (equivalentes a `cubic-bezier` de CSS).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Easing {
    Linear,
    EaseOut,
    EaseInOut,
    /// `cubic-bezier(.34, 1.56, .64, 1)`: el rebote del toast de la web.
    BackOut,
}

pub fn ease(t: f32, easing: Easing) -> f32 {
    let t = t.clamp(0.0, 1.0);
    match easing {
        Easing::Linear => t,
        Easing::EaseOut => 1.0 - (1.0 - t).powi(3),
        Easing::EaseInOut => {
            if t < 0.5 {
                4.0 * t * t * t
            } else {
                1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
            }
        }
        Easing::BackOut => {
            const C1: f32 = 1.70158;
            const C3: f32 = C1 + 1.0;
            let u = t - 1.0;
            1.0 + C3 * u * u * u + C1 * u * u
        }
    }
}

/// Interpolación suave de un valor hacia un objetivo.
pub fn approach(ctx: &egui::Context, id: egui::Id, target: f32, seconds: f32) -> f32 {
    ctx.animate_value_with_time(id, target, seconds)
}

/// Elevación al pasar el ratón (devuelve el desplazamiento vertical, negativo).
pub fn hover_lift(ctx: &egui::Context, id: egui::Id, hovered: bool, amount: f32) -> f32 {
    let t = ctx.animate_bool_with_time(id, hovered, 0.15);
    -amount * t
}

/// Sombra difusa (equivalente a `box-shadow`).
pub fn shadow(
    painter: &egui::Painter,
    rect: Rect,
    radius: f32,
    offset: [i8; 2],
    blur: u8,
    color: Color32,
) {
    if color.a() == 0 {
        return;
    }
    let shape = Shadow {
        offset,
        blur,
        spread: 0,
        color,
    }
    .as_shape(rect, radius);
    painter.add(egui::epaint::Shape::from(shape));
}

/// Opacidad pulsante (0.4 → 1.0) para puntos de "en vivo" y mining.
pub fn pulse(time: f64, period: f32) -> f32 {
    let phase = (time as f32 / period.max(0.01)) * std::f32::consts::TAU;
    0.4 + 0.6 * (0.5 + 0.5 * phase.sin())
}

/// Barra de progreso con degradado horizontal (`.progress-fill` de la web).
pub fn progress_bar(
    painter: &egui::Painter,
    rect: Rect,
    value: f32,
    from: Color32,
    to: Color32,
    track: Color32,
) {
    let radius = CornerRadius::same((rect.height() / 2.0).round() as u8);
    painter.rect_filled(rect, radius, track);
    let width = rect.width() * value.clamp(0.0, 1.0);
    if width < 1.0 {
        return;
    }
    let fill = Rect::from_min_size(rect.min, Vec2::new(width, rect.height()));
    // Degradado por franjas (egui no tiene shader de degradado).
    let steps = 10usize;
    for i in 0..steps {
        let t0 = i as f32 / steps as f32;
        let t1 = (i + 1) as f32 / steps as f32;
        let x0 = fill.min.x + fill.width() * t0;
        let x1 = fill.min.x + fill.width() * t1;
        let color = lerp_color(from, to, t0);
        painter.rect_filled(
            Rect::from_min_max(
                egui::pos2(x0, fill.min.y),
                egui::pos2(x1 + 0.5, fill.max.y),
            ),
            radius,
            color,
        );
    }
}

/// Interruptor con knob deslizante (`.tw-toggle` de la web).
pub fn switch(ui: &mut egui::Ui, id: egui::Id, on: bool, p: &Palette) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(42.0, 24.0), Sense::click());
    let t = ui.ctx().animate_bool_with_time(id, on, 0.2);
    let bg = lerp_color(p.surface3, p.accent, t);
    let border = lerp_color(p.border, p.accent, t);
    ui.painter().rect_filled(rect, CornerRadius::same(12), bg);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(12),
        egui::Stroke::new(1.5, border),
        egui::StrokeKind::Inside,
    );
    let knob_x = rect.min.x + 2.0 + t * (rect.width() - 20.0);
    ui.painter().circle_filled(
        egui::pos2(knob_x + 8.0, rect.center().y),
        8.0,
        Color32::WHITE,
    );
    response
}

/// Entrada escalonada de un elemento (`.event-item{animation:evIn .2s}`).
pub fn enter(ctx: &egui::Context, id: egui::Id, visible: bool) -> (f32, f32) {
    let t = ctx.animate_bool_with_time(id, visible, 0.2);
    (t, -4.0 * (1.0 - t))
}
