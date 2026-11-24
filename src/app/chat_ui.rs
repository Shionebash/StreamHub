//! Interfaz del chat (panel derecho).
//!
//! Se inspira en Chatterino: pestañas por canal, insignias, emotes como
//! imágenes, respuestas, autocompletado, historial y selector de emotes
//! flotante. Vive en un submódulo de `app` para acceder al estado privado sin
//! exponerlo al resto de la aplicación.

use super::*;
use crate::emote_cache;

/// Identificador estable del campo de escritura.
const INPUT_ID: &str = "chat_message_input";

fn input_id() -> egui::Id {
    egui::Id::unique(INPUT_ID)
}

/// Fragmento de un mensaje ya resuelto para pintarse.
pub(super) enum Seg {
    Time(String),
    User(String, egui::Color32),
    Text(String, egui::Color32, bool),
    Emote(String, String),
    Badge(String),
    Link(String, String),
    Space(f32),
}

/// Fila de chat pre-parseada y cacheada entre frames.
pub(super) struct CachedChatRow {
    pub(super) segments: Vec<Seg>,
    pub(super) mention: bool,
}

fn chat_event_key(event: &chat::ChatEvent) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (event.kind as u8).hash(&mut h);
    if !event.id.is_empty() {
        event.id.hash(&mut h);
    } else {
        event.at.hash(&mut h);
        event.login.hash(&mut h);
        event.text.hash(&mut h);
    }
    h.finish()
}

impl StreamHubApp {
    // -- estructura del panel ------------------------------------------------

    pub(super) fn chat_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = self.palette();
        // El chat nunca ocupa más de la mitad del ancho de la ventana.
        let max_width = (ctx.viewport_rect().width() * 0.52).clamp(320.0, 720.0);
        let target = self.cfg.chat_width.clamp(300.0, max_width);
        let width = anim::approach(
            ctx,
            egui::Id::unique("chat_panel_width"),
            if self.chat_panel { target } else { 0.0 },
            0.22,
        );
        if width < 12.0 {
            self.chat_drag = None;
            return;
        }
        let panel_alpha = (width / target.max(1.0)).clamp(0.0, 1.0);
        let response = egui::Panel::right("chat_panel")
            .resizable(false)
            .exact_size(width)
            .frame(
                egui::Frame::NONE
                    .fill(theme::chat_bg(&p))
                    .inner_margin(egui::Margin::symmetric(12, 10)),
            )
            .show(ui, |ui| {
                if panel_alpha < 0.99 {
                    ui.set_opacity(panel_alpha);
                }
                self.chat_panel_body(ui, ctx);
            });
        // Subtle left border separating chat from main canvas.
        let panel_rect = response.response.rect;
        ui.painter().line_segment(
            [
                egui::pos2(panel_rect.left(), panel_rect.top()),
                egui::pos2(panel_rect.left(), panel_rect.bottom()),
            ],
            egui::Stroke::new(1.0, p.border),
        );
        self.chat_drag_handle(ui, panel_rect);
    }

    /// Borde izquierdo del chat: arrastra para redimensionar el ancho.
    fn chat_drag_handle(&mut self, ui: &mut egui::Ui, panel: egui::Rect) {
        let handle = egui::Rect::from_min_size(
            egui::pos2(panel.left() - 3.0, panel.top()),
            egui::vec2(6.0, panel.height()),
        );
        egui::Area::new(egui::Id::unique("chat_drag_handle"))
            .fixed_pos(handle.min)
            .order(egui::Order::Middle)
            .show(ui.ctx(), |ui| {
                let (_, response) = ui.allocate_exact_size(handle.size(), egui::Sense::drag());
                response
                    .clone()
                    .on_hover_cursor(egui::CursorIcon::ResizeWest);
                if response.drag_started() {
                    let origin = response
                        .interact_pointer_pos()
                        .unwrap_or(handle.left_center());
                    self.chat_drag = Some((origin, self.cfg.chat_width));
                }
                if let Some((origin, base)) = self.chat_drag {
                    if let Some(pos) = ui.ctx().pointer_latest_pos() {
                        // Arrastrar hacia la izquierda agranda el chat.
                        let max_width =
                            (ui.ctx().viewport_rect().width() * 0.52).clamp(320.0, 720.0);
                        self.cfg.chat_width =
                            (base + (origin.x - pos.x)).clamp(300.0, max_width);
                    }
                    ui.ctx().request_repaint();
                    if response.drag_stopped() || !ui.ctx().input(|i| i.pointer.primary_down()) {
                        self.chat_drag = None;
                        let _ = self.cfg.save();
                    }
                }
            });
    }

    fn chat_panel_body(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.chat_panel_rect = Some(ui.max_rect());
        self.load_seven_global();
        self.load_seven_mine();
        // Escape cierra los paneles flotantes.
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.emote_picker = false;
            self.chat_settings_open = false;
        }

        if self.chat_channels.is_empty() {
            self.chat_empty_state(ui, ctx);
        } else {
            if self.chat_tab != ChatTab::Chat {
                self.emote_picker = false;
            }
            egui::Panel::top("chat_header")
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    self.chat_tabs(ui);
                    self.chat_toolbar(ui);
                    if self.chat_filter_open {
                        self.chat_filter_bar(ui);
                    }
                    ui.add_space(6.0);
                });
            egui::Panel::bottom("chat_footer")
                .frame(
                    egui::Frame::NONE
                        .inner_margin(egui::Margin { left: 0, right: 0, top: 4, bottom: 0 }),
                )
                .show(ui, |ui| self.chat_footer(ui));
            if self.chat_tab == ChatTab::Chat {
                egui::Panel::bottom("chat_input_panel")
                    .frame(
                        egui::Frame::NONE
                            .inner_margin(egui::Margin { left: 0, right: 0, top: 6, bottom: 0 }),
                    )
                    .show(ui, |ui| self.chat_input_area(ui, ctx));
            }
            let body_size = ui.available_size();
            ui.allocate_ui_with_layout(
                body_size,
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| {
                    ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                    match self.chat_tab {
                        ChatTab::Chat => self.chat_messages_view(ui),
                        ChatTab::Events => self.chat_events_view(ui),
                        ChatTab::Stats => self.chat_stats_view(ui),
                    }
                },
            );
        }

        self.chat_popups(ctx);
    }

    /// Estado vacío: lista de canales de la cola a los que unirse.
    fn chat_empty_state(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        let p = self.palette();
        ui.add_space(28.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(38.0, 38.0), egui::Sense::hover());
        icons::paint(ui.painter(), rect, Icon::Chat, p.dim, p.surface);
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new("Chat sin canales")
                .size(14.0)
                .strong()
                .color(p.text),
        );
        ui.label(
            egui::RichText::new("Únete a un canal para leer y escribir.")
                .size(11.0)
                .color(p.muted),
        );
        ui.add_space(12.0);

        let mut channels: Vec<String> = self
            .queue
            .iter()
            .map(|channel| channel.name.to_lowercase())
            .collect();
        for channel in &self.cfg.favorites {
            let channel = channel.to_lowercase();
            if !channels.contains(&channel) {
                channels.push(channel);
            }
        }
        channels.truncate(8);
        if channels.is_empty() {
            ui.label(
                egui::RichText::new("Añade canales a la cola o a favoritos.")
                    .size(11.0)
                    .color(p.dim),
            );
        }
        for channel in channels {
            let response = egui::Frame::NONE
                .fill(p.surface2)
                .corner_radius(egui::CornerRadius::same(7))
                .inner_margin(egui::Margin::symmetric(8, 5))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        self.avatar(ui, &channel, 18.0);
                        ui.label(
                            egui::RichText::new(format!("#{channel}"))
                                .size(11.5)
                                .color(p.text),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new("unirse")
                                    .size(10.0)
                                    .color(p.accent),
                            );
                        });
                    });
                })
                .response
                .interact(egui::Sense::click());
            if response.clicked() {
                self.chat_join(&channel);
            }
            ui.add_space(4.0);
        }
    }

    // -- pestañas y barra de herramientas ------------------------------------

    fn chat_tabs(&mut self, ui: &mut egui::Ui) {
        let p = self.palette();
        let channels = self.chat_channels.clone();
        let mut select: Option<String> = None;
        let mut leave: Option<String> = None;
        egui::ScrollArea::horizontal()
            .id_salt("chat_tabs")
            .max_height(32.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for channel in &channels {
                        let selected = self.chat_channel.as_deref() == Some(channel.as_str());
                        let unread = self.chat_unread.get(channel).copied().unwrap_or(0);
                        let is_live = self
                            .live
                            .get(channel)
                            .map(|info| info.live)
                            .unwrap_or(false);
                        let mut close_rect = egui::Rect::NOTHING;
                        let frame = egui::Frame::NONE
                            .fill(if selected {
                                p.accent_weak
                            } else {
                                p.surface2
                            })
                            .stroke(egui::Stroke::new(
                                1.0,
                                if selected { p.accent_mid } else { p.border },
                            ))
                            .corner_radius(egui::CornerRadius::same(14))
                            .inner_margin(egui::Margin::symmetric(10, 4))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 4.0;
                                    if is_live {
                                        let (dot, _) = ui.allocate_exact_size(
                                            egui::vec2(6.0, 6.0),
                                            egui::Sense::hover(),
                                        );
                                        let alpha =
                                            anim::pulse(ui.input(|input| input.time), 2.0);
                                        ui.painter().circle_filled(
                                            dot.center(),
                                            3.0,
                                            theme::with_alpha(p.red, alpha),
                                        );
                                    }
                                    ui.label(
                                        egui::RichText::new(format!("#{channel}"))
                                            .size(11.0)
                                            .strong()
                                            .color(if selected { p.accent } else { p.muted }),
                                    );
                                    if unread > 0 {
                                        let (rect, _) = ui.allocate_exact_size(
                                            egui::vec2(14.0, 14.0),
                                            egui::Sense::hover(),
                                        );
                                        ui.painter().circle_filled(rect.center(), 7.0, p.accent);
                                        ui.painter().text(
                                            rect.center(),
                                            egui::Align2::CENTER_CENTER,
                                            if unread > 9 {
                                                "9+".to_string()
                                            } else {
                                                unread.to_string()
                                            },
                                            egui::FontId::new(
                                                8.0,
                                                egui::FontFamily::Proportional,
                                            ),
                                            p.on_accent,
                                        );
                                    }
                                    let close = self.mini_icon(
                                        ui,
                                        Icon::Close,
                                        "Salir del canal",
                                        if selected { p.accent } else { p.dim },
                                        &p,
                                    );
                                    close_rect = close.rect;
                                    if close.clicked() {
                                        leave = Some(channel.clone());
                                    }
                                });
                            });
                        if frame.response.interact(egui::Sense::click()).clicked() {
                            // Si el clic cayó sobre la X se cierra el canal;
                            // si no, se selecciona.
                            let over_close = frame
                                .response
                                .interact_pointer_pos()
                                .map(|pos| close_rect.contains(pos))
                                .unwrap_or(false);                            if over_close {
                                leave = Some(channel.clone());
                            } else {
                                select = Some(channel.clone());
                            }
                        }
                    }
                    // Añadir un canal de la cola/favoritos.
                    ui.menu_button("＋", |ui| {
                        let mut joined = false;
                        let candidates: Vec<String> = self
                            .queue
                            .iter()
                            .map(|channel| channel.name.to_lowercase())
                            .chain(self.cfg.favorites.iter().map(|name| name.to_lowercase()))
                            .filter(|name| !self.chat_channels.contains(name))
                            .collect();
                        for channel in candidates {
                            if ui.button(format!("#{channel}")).clicked() {
                                select = Some(channel.clone());
                                joined = true;
                                ui.close();
                            }
                        }
                        if !joined && self.chat_channels.len() == self.queue.len() {
                            ui.label(
                                egui::RichText::new("Todos los canales están abiertos")
                                    .size(10.0)
                                    .color(p.dim),
                            );
                        }
                    });
                });
            });
        if let Some(channel) = leave {
            self.chat_leave(&channel);
        } else if let Some(channel) = select {
            if !self.chat_channels.contains(&channel) {
                self.chat_join(&channel);
            }
            self.chat_channel = Some(channel.clone());
            self.chat_unread.remove(&channel);
            self.chat_follow = true;
            self.chat_completion = None;
        }
    }

    /// Control segmentado con pulgar deslizante, como el de la referencia.
    /// Devuelve el índice pulsado, si lo hay.
    fn segmented_index(
        &mut self,
        ui: &mut egui::Ui,
        salt: &str,
        labels: &[&str],
        selected: usize,
        p: &Palette,
        height: f32,
    ) -> Option<usize> {
        if labels.is_empty() {
            return None;
        }
        let width = ui.available_width().max(40.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
        if !ui.is_rect_visible(rect) {
            return None;
        }
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(12), p.surface2);
        ui.painter().rect_stroke(
            rect,
            egui::CornerRadius::same(12),
            egui::Stroke::new(1.0, p.border),
            egui::StrokeKind::Inside,
        );
        let count = labels.len() as f32;
        let seg_w = rect.width() / count;
        let id = egui::Id::unique(salt);
        let t = anim::approach(ui.ctx(), id, selected as f32, 0.18);
        let thumb = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 3.0 + seg_w * t, rect.top() + 3.0),
            egui::vec2((seg_w - 6.0).max(8.0), height - 6.0),
        );
        anim::shadow(
            ui.painter(),
            thumb,
            9.0,
            [0, 1],
            4,
            egui::Color32::from_black_alpha(20),
        );
        ui.painter()
            .rect_filled(thumb, egui::CornerRadius::same(9), p.surface);
        ui.painter().rect_stroke(
            thumb,
            egui::CornerRadius::same(9),
            egui::Stroke::new(1.0, p.border),
            egui::StrokeKind::Inside,
        );
        let mut clicked = None;
        for (index, label) in labels.iter().enumerate() {
            let area = egui::Rect::from_min_size(
                egui::pos2(rect.left() + seg_w * index as f32, rect.top()),
                egui::vec2(seg_w, height),
            );
            let response = ui
                .interact(area, id.with(index), egui::Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            let active = index == selected;
            let color = if active {
                p.accent
            } else if response.hovered() {
                p.text
            } else {
                p.muted
            };
            ui.painter().text(
                area.center(),
                egui::Align2::CENTER_CENTER,
                *label,
                egui::FontId::new(10.5, egui::FontFamily::Proportional),
                color,
            );
            if response.clicked() {
                clicked = Some(index);
            }
        }
        clicked
    }

    fn chat_toolbar(&mut self, ui: &mut egui::Ui) {
        let p = self.palette();
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let connected = self.chat_connected;
            let (dot_label, dot_color) = if connected {
                ("conectado", p.green)
            } else {
                ("conectando", p.gold)
            };
            egui::Frame::NONE
                .fill(theme::with_alpha(dot_color, 0.14))
                .corner_radius(egui::CornerRadius::same(10))
                .inner_margin(egui::Margin::symmetric(8, 2))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 5.0;
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(7.0, 7.0), egui::Sense::hover());
                        let pulse = if connected {
                            anim::pulse(ui.input(|input| input.time), 2.4)
                        } else {
                            1.0
                        };
                        ui.painter().circle_filled(
                            rect.center(),
                            3.0,
                            theme::with_alpha(dot_color, pulse),
                        );
                        ui.label(
                            egui::RichText::new(dot_label)
                                .size(10.0)
                                .color(dot_color),
                        );
                    });
                });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if self
                    .mini_icon(ui, Icon::Close, "Ocultar panel (sigue conectado)", p.muted, &p)
                    .clicked()
                {
                    self.chat_panel = false;
                }
                if self
                    .mini_icon(
                        ui,
                        Icon::Settings,
                        "Ajustes y opciones del chat",
                        if self.chat_settings_open { p.accent } else { p.muted },
                        &p,
                    )
                    .clicked()
                {
                    self.chat_settings_open = !self.chat_settings_open;
                }
                if self
                    .mini_icon(ui, Icon::Refresh, "Recargar historial", p.muted, &p)
                    .clicked()
                {
                    if let Some(channel) = self.chat_channel.clone() {
                        self.chat_history_requested.remove(&channel);
                        self.request_history(&channel);
                        self.set_toast("Cargando historial reciente...");
                    }
                }
                if self
                    .mini_icon(
                        ui,
                        Icon::Search,
                        "Filtrar mensajes",
                        if self.chat_filter_open { p.accent } else { p.muted },
                        &p,
                    )
                    .clicked()
                {
                    self.chat_filter_open = !self.chat_filter_open;
                    if !self.chat_filter_open {
                        self.chat_filter.clear();
                    }
                }
            });
        });

        ui.add_space(6.0);
        let selected = match self.chat_tab {
            ChatTab::Chat => 0,
            ChatTab::Events => 1,
            ChatTab::Stats => 2,
        };
        if let Some(index) = self.segmented_index(
            ui,
            "chat_content_tabs",
            &["Chat", "Eventos", "Estadísticas"],
            selected,
            &p,
            28.0,
        ) {
            let next = [ChatTab::Chat, ChatTab::Events, ChatTab::Stats][index];
            if next != self.chat_tab {
                self.chat_tab = next;
                if next != ChatTab::Chat {
                    self.emote_picker = false;
                    self.chat_completion = None;
                }
                ui.ctx().request_repaint();
            }
        }
        ui.add_space(2.0);
    }

    fn chat_filter_bar(&mut self, ui: &mut egui::Ui) {
        let p = self.palette();
        ui.horizontal(|ui| {
            let response = ui.add_sized(
                [ui.available_width() - 26.0, 24.0],
                egui::TextEdit::singleline(&mut self.chat_filter)
                    .hint_text("Filtrar por texto o usuario...")
                    .text_color(p.text),
            );
            if response.changed() {
                self.chat_follow = true;
            }
            if self.mini_icon(ui, Icon::Close, "Quitar filtro", p.muted, &p).clicked() {
                self.chat_filter.clear();
                self.chat_filter_open = false;
            }
        });
    }

    fn chat_footer(&mut self, ui: &mut egui::Ui) {
        let p = self.palette();
        let channel = self.chat_channel.clone().unwrap_or_default();
        let key = channel.to_lowercase();
        let stats = self.chat_stats.get(&channel).copied().unwrap_or_default();
        let balance = self.mining_points.get(&key).copied();
        let watching = self.embedded_channel.as_deref() == Some(key.as_str())
            || self.running.keys().any(|k| k.eq_ignore_ascii_case(&key));
        let pending_chest = self
            .mining_chests
            .iter()
            .find(|c| c.channel.eq_ignore_ascii_case(&key))
            .cloned();

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;

            if !key.is_empty() && self.has_creds() {
                let pts_label = match balance {
                    Some(b) => super::format_count(b),
                    None => "Puntos".to_string(),
                };
                let fill = if self.points_popup_open {
                    p.accent_weak
                } else {
                    p.surface2
                };
                let stroke_col = if self.points_popup_open {
                    p.accent
                } else {
                    p.border2
                };
                let pill = egui::Frame::NONE
                    .fill(fill)
                    .stroke(egui::Stroke::new(1.0, stroke_col))
                    .corner_radius(egui::CornerRadius::same(10))
                    .inner_margin(egui::Margin::symmetric(7, 2))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            let (icon_rect, _) = ui.allocate_exact_size(
                                egui::vec2(11.0, 11.0),
                                egui::Sense::hover(),
                            );
                            icons::paint(
                                ui.painter(),
                                icon_rect,
                                Icon::Gem,
                                if self.points_popup_open { p.accent } else { p.gold },
                                fill,
                            );
                            ui.label(
                                egui::RichText::new(&pts_label)
                                    .size(9.5)
                                    .strong()
                                    .color(if self.points_popup_open { p.accent } else { p.text }),
                            );
                            if watching && self.mining_running {
                                let pulse = anim::pulse(ui.input(|i| i.time), 2.2);
                                let (dot_rect, _) = ui.allocate_exact_size(
                                    egui::vec2(6.0, 6.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().circle_filled(
                                    dot_rect.center(),
                                    3.0,
                                    theme::with_alpha(p.green, pulse),
                                );
                            }
                        });
                    });
                let click = pill.response.interact(egui::Sense::click());
                let tip = if watching {
                    "Puntos del canal (ganando automáticamente · clic para canjear recompensas)"
                } else {
                    "Puntos del canal (clic para ver y canjear recompensas)"
                };
                if click.on_hover_text(tip).clicked() {
                    self.points_popup_open = !self.points_popup_open;
                    if self.points_popup_open {
                        self.emote_picker = false;
                        self.points_selected_reward = None;
                        self.points_reward_input.clear();
                        self.mining_refresh_channel(&key);
                    }
                }

                if let Some(chest) = pending_chest {
                    let chest_btn = egui::Button::new(
                        egui::RichText::new(format!("+{}", chest.points))
                            .size(9.5)
                            .strong()
                            .color(p.on_accent),
                    )
                    .fill(p.gold)
                    .corner_radius(egui::CornerRadius::same(10));
                    if ui
                        .add(chest_btn)
                        .on_hover_text("Reclamar cofre de puntos ahora")
                        .clicked()
                    {
                        let _ = self.mining_tx.send(crate::mining::Command::Claim(chest));
                    }
                }
            }

            if stats.messages > 0 {
                ui.label(
                    egui::RichText::new(format!("{} msgs", stats.messages))
                        .size(9.0)
                        .color(p.dim),
                );
            }
            if stats.bits > 0 {
                ui.label(
                    egui::RichText::new(format!("· {} bits", stats.bits))
                        .size(9.0)
                        .color(p.gold),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let count = self.chat_input.chars().count();
                if count > 0 {
                    let color = if count > 500 { p.red } else { p.dim };
                    ui.label(
                        egui::RichText::new(format!("{count}/500"))
                            .size(9.0)
                            .color(color),
                    );
                }
            });
        });
    }

    // -- mensajes ------------------------------------------------------------

    fn chat_messages_view(&mut self, ui: &mut egui::Ui) {
        let p = self.palette();
        let Some(channel) = self.chat_channel.clone() else {
            return;
        };
        if self.chat_channels.is_empty() {
            return;
        }
        // Estos valores se calculan una vez por frame: antes se repetían por
        // cada mensaje visible (hasta 150 veces), incluida la carpeta de caché.
        let emote_index = self.emote_index_for(&channel);
        let cache_dir = self.channel_cache_dir(&channel);
        let my_login = self.my_login.to_lowercase();
        let mention_needle = format!("@{my_login}");
        let font_size = self.cfg.chat_font.clamp(10.0, 18.0);
        let filter = self.chat_filter.trim();
        let filter_empty = filter.is_empty();
        let mut indices: Vec<usize> = self
            .chat_messages
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, event)| event.channel.is_empty() || event.channel == channel)
            .filter(|(_, event)| {
                filter_empty
                    || contains_ignore_case(&event.login, filter)
                    || contains_ignore_case(&event.text, filter)
                    || contains_ignore_case(&event.display, filter)
            })
            .take(150)
            .map(|(index, _)| index)
            .collect();
        indices.reverse();
        let shown = indices.as_slice();

        // Subir con la rueda desactiva el seguimiento automático.
        let area_rect = ui.available_rect_before_wrap();
        if ui.rect_contains_pointer(area_rect) {
            let scroll = ui.input(|input| input.smooth_scroll_delta.y);
            if scroll > 0.0 {
                self.chat_follow = false;
            }
        }

        let seg_key = (
            channel.clone(),
            self.emote_generation,
            self.cfg.chat_time,
            self.cfg.chat_badges,
            self.cfg.theme,
            my_login.clone(),
        );
        if self.chat_seg_key != seg_key || self.chat_seg_cache.len() > 600 {
            self.chat_seg_cache.clear();
            self.chat_seg_key = seg_key;
        }

        let out = egui::ScrollArea::vertical()
            .id_salt(("chat_scroll", &channel))
            .auto_shrink([false, false])
            .stick_to_bottom(self.chat_follow)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                if shown.is_empty() {
                    ui.add_space(18.0);
                    ui.label(
                        egui::RichText::new(if filter_empty {
                            "Esperando mensajes..."
                        } else {
                            "Ningún mensaje coincide con el filtro"
                        })
                        .size(11.0)
                        .color(p.dim),
                    );
                }
                let avail_w = ui.available_width().max(1.0);
                let layout_key = (
                    channel.clone(),
                    avail_w.round() as u32,
                    (font_size * 10.0).round() as u32,
                    self.cfg.chat_time,
                    self.cfg.chat_badges,
                    self.emote_generation,
                );
                if self.chat_layout_key != layout_key || self.chat_row_heights.len() > 600 {
                    self.chat_row_heights.clear();
                    self.chat_layout_key = layout_key;
                }
                let clip_rect = ui.clip_rect();
                for (row, index) in shown.iter().enumerate() {
                    let key = chat_event_key(&self.chat_messages[*index]);
                    // Virtualización por viewport: si la fila ya fue medida y está
                    // fuera del área visible (+80px de margen), solo reservamos su
                    // altura sin crear widgets ni actualizar emotes animados.
                    if let Some(&cached_h) = self.chat_row_heights.get(&key) {
                        let y = ui.cursor().top();
                        if y + cached_h < clip_rect.min.y - 80.0 || y > clip_rect.max.y + 80.0 {
                            ui.allocate_space(egui::vec2(avail_w, cached_h));
                            continue;
                        }
                    }
                    // Se saca el evento de la lista en lugar de clonarlo: los
                    // mensajes tienen muchos `String` y el clon por frame era
                    // una de las causas de la lentitud del chat.
                    let event = std::mem::take(&mut self.chat_messages[*index]);
                    let cached = if let Some(hit) = self.chat_seg_cache.get(&key) {
                        Arc::clone(hit)
                    } else {
                        let built = Arc::new(self.build_chat_row(
                            &event,
                            &emote_index,
                            &my_login,
                            &mention_needle,
                        ));
                        self.chat_seg_cache.insert(key, Arc::clone(&built));
                        built
                    };
                    let measured_h = self.chat_message_row(
                        ui,
                        &event,
                        &cached,
                        row,
                        &cache_dir,
                        font_size,
                    );
                    self.chat_messages[*index] = event;
                    self.chat_row_heights.insert(key, measured_h);
                }
            });

        let at_bottom =
            out.state.clamped_offset().y + out.inner_rect.height() >= out.content_size.y - 8.0;
        let pointer_dragging_chat = ui.rect_contains_pointer(out.inner_rect)
            && ui.input(|input| input.pointer.primary_down());
        if at_bottom {
            self.chat_follow = true;
            self.chat_unread.remove(&channel);
        } else if self.chat_follow && pointer_dragging_chat {
            self.chat_follow = false;
        }

        // Botón flotante para volver al final cuando hay mensajes nuevos.
        if !self.chat_follow {
            let unread = self.chat_unread.get(&channel).copied().unwrap_or(0);
            let label = if unread > 0 {
                format!("↓ {unread} nuevos")
            } else {
                "↓ Ir al final".to_string()
            };
            let button_rect = egui::Rect::from_center_size(
                egui::pos2(out.inner_rect.center().x, out.inner_rect.max.y - 16.0),
                egui::vec2(108.0, 24.0),
            );
            let button = egui::Button::new(egui::RichText::new(label).size(10.0).color(p.on_accent))
                .fill(p.accent)
                .corner_radius(egui::CornerRadius::same(12));
            if ui.put(button_rect, button).clicked() {
                self.chat_follow = true;
                self.chat_unread.remove(&channel);
                ui.ctx().request_repaint();
            }
        }
    }

    fn build_chat_row(
        &self,
        event: &chat::ChatEvent,
        emote_index: &std::collections::HashMap<String, String>,
        my_login: &str,
        mention_needle: &str,
    ) -> CachedChatRow {
        let p = self.palette();
        let mention = !my_login.is_empty()
            && event.kind == chat::Kind::Message
            && contains_ignore_case(&event.text, mention_needle);
        let mine = !my_login.is_empty() && event.login == my_login;
        let is_action = event.kind == chat::Kind::Action;

        let mut segments: Vec<Seg> = Vec::new();
        if self.cfg.chat_time {
            let time = event
                .at
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| {
                    let secs = value.as_secs() % 86_400;
                    format!("{:02}:{:02}", secs / 3600, (secs % 3600) / 60)
                })
                .unwrap_or_default();
            segments.push(Seg::Time(time));
            segments.push(Seg::Space(5.0));
        }

        match event.kind {
            chat::Kind::System => {
                if !event.display.is_empty() {
                    segments.push(Seg::Text(format!("{} ", event.display), p.gold, false));
                }
                segments.push(Seg::Text(event.text.clone(), p.gold, false));
            }
            chat::Kind::Notice => {
                segments.push(Seg::Text(event.text.clone(), p.dim, true));
            }
            chat::Kind::Whisper => {
                segments.push(Seg::Badge("whisper/1".to_string()));
                segments.push(Seg::Text(format!("{} ", event.display), p.gold, false));
                segments.push(Seg::Text(event.text.clone(), p.text, false));
            }
            chat::Kind::Action => {
                segments.push(Seg::Text(format!("{} ", event.display), p.accent, true));
                self.push_message_emotes(
                    &mut segments,
                    event,
                    p.accent,
                    true,
                    my_login,
                    emote_index,
                );
            }
            chat::Kind::Message => {
                if self.cfg.chat_badges {
                    for badge in &event.badges {
                        segments.push(Seg::Badge(badge.clone()));
                    }
                }
                let raw_color = if mine {
                    p.accent
                } else {
                    event
                        .color
                        .as_deref()
                        .and_then(parse_hex_color)
                        .unwrap_or(p.accent)
                };
                let color = readable_chat_color(raw_color, &p);
                segments.push(Seg::User(event.display.clone(), color));
                if is_action {
                    segments.push(Seg::Text(" ".to_string(), p.text, false));
                } else {
                    segments.push(Seg::Text(": ".to_string(), p.dim, false));
                }
                self.push_message_emotes(
                    &mut segments,
                    event,
                    p.text,
                    false,
                    my_login,
                    emote_index,
                );
                if event.bits > 0 {
                    segments.push(Seg::Space(4.0));
                    segments.push(Seg::Text(
                        format!("{} bits", event.bits),
                        p.blue,
                        false,
                    ));
                }
            }
            chat::Kind::Status | chat::Kind::EmoteSets => {}
            chat::Kind::Delete | chat::Kind::Clear => {
                segments.push(Seg::Text(event.text.clone(), p.red, true));
            }
        }

        CachedChatRow { segments, mention }
    }

    #[allow(clippy::too_many_arguments)]
    fn chat_message_row(
        &mut self,
        ui: &mut egui::Ui,
        event: &chat::ChatEvent,
        cached: &CachedChatRow,
        row: usize,
        cache_dir: &std::path::Path,
        font_size: f32,
    ) -> f32 {
        let p = self.palette();
        let mention = cached.mention;

        let mut frame = egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(10, 5))
            .corner_radius(egui::CornerRadius::same(10));
        if mention {
            frame = frame.fill(p.accent_weak);
        } else if self.cfg.chat_alt && row % 2 == 1 {
            frame = frame.fill(theme::with_alpha(p.surface2, 0.30));
        }

        let inner = frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            if let Some((login, snippet)) = &event.reply {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let (bar_rect, _) =
                        ui.allocate_exact_size(egui::vec2(2.5, 11.0), egui::Sense::hover());
                    ui.painter().rect_filled(
                        bar_rect,
                        egui::CornerRadius::same(2),
                        p.accent_mid,
                    );
                    ui.label(
                        egui::RichText::new(format!("@{login}"))
                            .size(9.5)
                            .strong()
                            .color(p.accent),
                    );
                    if !snippet.is_empty() {
                        ui.label(
                            egui::RichText::new(snippet)
                                .size(9.5)
                                .color(p.dim)
                                .italics(),
                        );
                    }
                });
                ui.add_space(2.0);
            }
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 3.0);
                for segment in &cached.segments {
                    match segment {
                        Seg::Time(time) => {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(time.as_str())
                                        .size((font_size - 2.5).max(9.0))
                                        .monospace()
                                        .color(p.dim),
                                )
                                .selectable(false),
                            );
                        }
                        Seg::User(name, color) => {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(name.as_str())
                                        .size(font_size)
                                        .strong()
                                        .color(*color)
                                        .family(egui::FontFamily::Proportional),
                                )
                                .selectable(false),
                            );
                        }
                        Seg::Text(text, color, italics) => {
                            let mut rich = egui::RichText::new(text.as_str())
                                .size(font_size)
                                .color(*color)
                                .family(egui::FontFamily::Proportional);
                            if *italics {
                                rich = rich.italics();
                            }
                            ui.add(egui::Label::new(rich).selectable(false));
                        }
                        Seg::Emote(url, name) => {
                            let size = font_size + 6.0;
                            if let Some(texture) =
                                self.images
                                    .get_cached(ui.ctx(), url.as_str(), 24, 24, Some(cache_dir))
                            {
                                ui.add(
                                    egui::Image::new((texture.id(), egui::vec2(size, size)))
                                        .corner_radius(egui::CornerRadius::same(3)),
                                )
                                .on_hover_text(name.as_str());
                            } else {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(name.as_str())
                                            .size(font_size)
                                            .color(p.dim),
                                    )
                                    .selectable(false),
                                );
                            }
                        }
                        Seg::Badge(badge) => self.badge_chip(ui, badge.as_str()),
                        Seg::Link(label, url) => {
                            let response = ui.add(
                                egui::Label::new(
                                    egui::RichText::new(label.as_str())
                                        .size(font_size)
                                        .color(p.blue)
                                        .underline(),
                                )
                                .selectable(false)
                                .sense(egui::Sense::click()),
                            );
                            if response.clicked() {
                                open_external(url.as_str());
                            }
                        }
                        Seg::Space(amount) => ui.add_space(*amount),
                    }
                }
            });
        });

        let response = inner.response.interact(egui::Sense::click());
        if mention {
            let bar = egui::Rect::from_min_size(
                egui::pos2(response.rect.left() + 1.0, response.rect.top() + 3.0),
                egui::vec2(2.5, (response.rect.height() - 6.0).max(2.0)),
            );
            ui.painter()
                .rect_filled(bar, egui::CornerRadius::same(2), p.accent);
        }
        if response.hovered() {
            ui.painter().rect_filled(
                response.rect,
                egui::CornerRadius::same(10),
                theme::with_alpha(p.accent, 0.06),
            );
            if !mention {
                let bar = egui::Rect::from_min_size(
                    egui::pos2(response.rect.left() + 1.0, response.rect.top() + 4.0),
                    egui::vec2(2.0, (response.rect.height() - 8.0).max(2.0)),
                );
                ui.painter().rect_filled(
                    bar,
                    egui::CornerRadius::same(2),
                    theme::with_alpha(p.accent, 0.55),
                );
            }
            ui.ctx().set_cursor_icon(egui::CursorIcon::Default);
        }
        if response.clicked() && !event.login.is_empty() {
            self.insert_text(&format!("@{} ", event.login));
            self.chat_focus_input = true;
        }
        response.context_menu(|ui| {
            ui.set_min_width(180.0);
            if !event.login.is_empty() {
                if ui.button("Responder").clicked() {
                    if !event.id.is_empty() {
                        self.chat_reply = Some((
                            event.channel.clone(),
                            event.id.clone(),
                            event.display.clone(),
                        ));
                    } else {
                        self.insert_text(&format!("@{} ", event.login));
                    }
                    self.chat_focus_input = true;
                    ui.close();
                }
                if ui.button("Mencionar").clicked() {
                    self.insert_text(&format!("@{} ", event.login));
                    self.chat_focus_input = true;
                    ui.close();
                }
                if ui.button("Filtrar por este usuario").clicked() {
                    self.chat_filter = event.login.clone();
                    self.chat_filter_open = true;
                    ui.close();
                }
            }
            if ui.button("Copiar mensaje").clicked() {
                ui.ctx().copy_text(event.text.clone());
                ui.close();
            }
            if ui.button("Copiar usuario").clicked() {
                ui.ctx().copy_text(event.display.clone());
                ui.close();
            }
            if !event.login.is_empty() && ui.button("Abrir perfil en Twitch").clicked() {
                open_external(&format!("https://twitch.tv/{}", event.login));
                ui.close();
            }
        });
        response.rect.height()
    }

    /// Texto del mensaje teniendo en cuenta los emotes nativos de Twitch.
    fn push_message_emotes(
        &self,
        out: &mut Vec<Seg>,
        event: &chat::ChatEvent,
        base: egui::Color32,
        italics: bool,
        my_login: &str,
        emote_index: &std::collections::HashMap<String, String>,
    ) {
        let mut cursor = 0usize;
        for emote in &event.emotes {
            if emote.start < cursor || emote.end > event.text.len() {
                continue;
            }
            if emote.start > cursor {
                self.push_message_text(
                    out,
                    &event.text[cursor..emote.start],
                    base,
                    italics,
                    my_login,
                    emote_index,
                );
            }
            let name = event.text[emote.start..emote.end].to_string();
            out.push(Seg::Emote(
                format!(
                    "https://static-cdn.jtvnw.net/emoticons/v2/{}/default/dark/2.0",
                    emote.id
                ),
                name,
            ));
            cursor = emote.end;
        }
        if cursor < event.text.len() {
            self.push_message_text(
                out,
                &event.text[cursor..],
                base,
                italics,
                my_login,
                emote_index,
            );
        }
    }

    /// Convierte el texto en segmentos, sustituyendo emotes por imágenes.
    ///
    /// Los trozos de texto normales se agrupan en un solo segmento para que
    /// egui cree muchas menos etiquetas por frame (clave para la memoria).
    fn push_message_text(
        &self,
        out: &mut Vec<Seg>,
        text: &str,
        base: egui::Color32,
        italics: bool,
        my_login: &str,
        emote_index: &std::collections::HashMap<String, String>,
    ) {
        let mut buffer = String::new();
        let flush = |out: &mut Vec<Seg>, buffer: &mut String| {
            if !buffer.is_empty() {
                out.push(Seg::Text(std::mem::take(buffer), base, italics));
            }
        };
        for word in text.split_inclusive(' ') {
            if word.is_empty() {
                continue;
            }
            let trimmed = word.trim_end();
            let key: std::borrow::Cow<str> =
                if trimmed.bytes().any(|byte| byte.is_ascii_uppercase()) {
                    // Solo se reserva memoria cuando hay mayúsculas; la
                    // mayoría de palabras del chat ya vienen en minúsculas.
                    std::borrow::Cow::Owned(trimmed.to_lowercase())
                } else {
                    std::borrow::Cow::Borrowed(trimmed)
                };
            if key.is_empty() {
                buffer.push_str(word);
                continue;
            }
            let emote = emote_index.get(key.as_ref()).cloned();
            if let Some(url) = emote {
                flush(out, &mut buffer);
                out.push(Seg::Emote(url, trimmed.to_string()));
                if word.ends_with(' ') {
                    buffer.push(' ');
                }
            } else if key.starts_with("http://") || key.starts_with("https://") {
                flush(out, &mut buffer);
                out.push(Seg::Link(trimmed.to_string(), trimmed.to_string()));
                if word.ends_with(' ') {
                    buffer.push(' ');
                }
            } else if key.starts_with('@') {
                flush(out, &mut buffer);
                let color = if !my_login.is_empty() && key.contains(my_login) {
                    self.palette().accent
                } else {
                    self.palette().blue
                };
                out.push(Seg::Text(word.to_string(), color, italics));
            } else {
                buffer.push_str(word);
            }
        }
        flush(out, &mut buffer);
    }

    fn badge_chip(&self, ui: &mut egui::Ui, badge: &str) {
        let p = self.palette();
        let (label, color) = badge_style(badge, &p);
        let font = egui::FontId::new(8.0, egui::FontFamily::Proportional);
        let galley = ui.painter().layout_no_wrap(label, font, color);
        let width = galley.size().x + 10.0;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 16.0), egui::Sense::hover());
        let chip = egui::Rect::from_center_size(rect.center(), egui::vec2(width, 14.0));
        ui.painter().rect_filled(
            chip,
            egui::CornerRadius::same(5),
            theme::with_alpha(color, 0.14),
        );
        ui.painter().rect_stroke(
            chip,
            egui::CornerRadius::same(5),
            egui::Stroke::new(0.75, theme::with_alpha(color, 0.35)),
            egui::StrokeKind::Inside,
        );
        ui.painter()
            .galley(chip.center() - galley.size() / 2.0, galley, color);
        ui.add_space(3.0);
    }

    // -- entrada de texto ----------------------------------------------------

    fn chat_input_area(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = self.palette();
        if let Some((_, _, display)) = self.chat_reply.clone() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(
                    egui::RichText::new(format!("Respondiendo a {display}"))
                        .size(9.5)
                        .color(p.accent),
                );
                if self.mini_icon(ui, Icon::Close, "Cancelar respuesta", p.dim, &p).clicked() {
                    self.chat_reply = None;
                }
            });
        }

        let id = input_id();
        let focused = ui.memory(|memory| memory.has_focus(id));

        // Atajos: historial y autocompletado. Se consumen antes del TextEdit.
        if focused {
            if ui
                .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp))
            {
                self.history_nav(-1);
            }
            if ui
                .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown))
            {
                self.history_nav(1);
            }
            if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Tab)) {
                self.complete_input(ctx);
            }
        }

        let focused_now = ui.memory(|memory| memory.has_focus(id));
        let mut trigger_send = false;
        let capsule = egui::Frame::NONE
            .fill(p.surface2)
            .stroke(egui::Stroke::new(
                1.0,
                if focused_now { p.accent } else { p.border2 },
            ))
            .corner_radius(egui::CornerRadius::same(20))
            .inner_margin(egui::Margin::symmetric(8, 4))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let (emote_rect, emote_res) =
                        ui.allocate_exact_size(egui::vec2(26.0, 26.0), egui::Sense::click());
                    let emote_hover = emote_res.hovered();
                    if emote_hover || self.emote_picker {
                        ui.painter().circle_filled(
                            emote_rect.center(),
                            13.0,
                            if self.emote_picker { p.accent_weak } else { p.surface3 },
                        );
                    }
                    icons::paint(
                        ui.painter(),
                        emote_rect.shrink(6.0),
                        Icon::Smile,
                        if self.emote_picker {
                            p.accent
                        } else if emote_hover {
                            p.text
                        } else {
                            p.muted
                        },
                        p.surface2,
                    );
                    if emote_res
                        .on_hover_text("Selector de emotes (Tab para completar)")
                        .clicked()
                    {
                        self.emote_picker = !self.emote_picker;
                        if self.emote_picker {
                            self.points_popup_open = false;
                        }
                    }

                    let input_width = (ui.available_width() - 36.0).max(60.0);
                    let response = ui.add_sized(
                        [input_width, 24.0],
                        egui::TextEdit::singleline(&mut self.chat_input)
                            .id(id)
                            .frame(egui::Frame::NONE)
                            .hint_text("Envía un mensaje o /help...")
                            .font(egui::FontId::new(
                                self.cfg.chat_font.clamp(10.0, 18.0),
                                egui::FontFamily::Proportional,
                            )),
                    );
                    if self.chat_focus_input {
                        response.request_focus();
                        self.chat_focus_input = false;
                    }
                    let enter = ui.input(|input| input.key_pressed(egui::Key::Enter));
                    let send_by_key = enter && (focused || response.lost_focus());

                    let (send_rect, send_res) =
                        ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::click());
                    let send_hover = send_res.hovered();
                    ui.painter().circle_filled(
                        send_rect.center(),
                        14.0,
                        if send_hover { p.accent_mid } else { p.accent },
                    );
                    icons::paint(
                        ui.painter(),
                        send_rect.shrink(8.0),
                        Icon::Play,
                        p.on_accent,
                        p.accent,
                    );
                    let send_clicked = send_res.on_hover_text("Enviar (Enter)").clicked();
                    trigger_send = send_by_key || send_clicked;
                    response
                })
                .inner
            });
        self.chat_input_rect = Some(capsule.response.rect);

        if trigger_send {
            self.chat_send();
            ui.ctx().request_repaint();
        }

        // Sugerencias de comandos.
        if self.chat_input.starts_with('/') && !self.chat_input.contains(' ') {
            self.command_hints(ui);
        }
    }

    fn command_hints(&mut self, ui: &mut egui::Ui) {
        let p = self.palette();
        let prefix = self.chat_input.to_lowercase();
        let commands: [(&str, &str); 9] = [
            ("/me", "acción en cursiva"),
            ("/w", "mensaje privado"),
            ("/help", "lista de comandos"),
            ("/clear", "limpiar el chat local"),
            ("/history", "recargar historial"),
            ("/ping", "latencia del chat"),
            ("/uptime", "tiempo en directo"),
            ("/color", "cambiar color del nombre"),
            ("/announce", "anuncio destacado"),
        ];
        let matches: Vec<(&str, &str)> = commands
            .iter()
            .filter(|(command, _)| command.starts_with(&prefix))
            .copied()
            .collect();
        if matches.is_empty() {
            return;
        }
        let Some(anchor) = self.chat_input_rect else {
            return;
        };
        let height = matches.len() as f32 * 20.0 + 12.0;
        egui::Area::new(egui::Id::unique("chat_command_hints"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(anchor.left(), anchor.top() - height - 4.0))
            .constrain(true)
            .show(ui.ctx(), |ui| {
                egui::Frame::NONE
                    .fill(p.surface)
                    .stroke(egui::Stroke::new(1.0, p.border2))
                    .corner_radius(egui::CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(8, 5))
                    .show(ui, |ui| {
                        ui.set_width(anchor.width().min(260.0));
                        for (command, description) in matches {
                            let response = ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format!(
                                        "{command}  ·  {description}"
                                    ))
                                    .size(10.5)
                                    .color(p.text),
                                )
                                .selectable(false)
                                .sense(egui::Sense::click()),
                            );
                            if response.clicked() {
                                self.chat_input = format!("{command} ");
                                self.chat_focus_input = true;
                            }
                        }
                    });
            });
    }

    fn history_nav(&mut self, direction: i32) {
        if self.chat_history.is_empty() {
            return;
        }
        let len = self.chat_history.len();
        let next = match (self.chat_history_pos, direction) {
            (None, -1) => Some(len - 1),
            (None, _) => None,
            (Some(0), -1) => Some(0),
            (Some(pos), -1) => Some(pos.saturating_sub(1)),
            (Some(pos), 1) if pos + 1 < len => Some(pos + 1),
            (Some(_), 1) => None,
            _ => None,
        };
        match next {
            Some(pos) => {
                self.chat_input = self.chat_history[pos].clone();
                self.chat_history_pos = Some(pos);
            }
            None => {
                self.chat_input.clear();
                self.chat_history_pos = None;
            }
        }
    }

    fn complete_input(&mut self, ctx: &egui::Context) {
        let cursor = egui::text_edit::TextEditState::load(ctx, input_id())
            .and_then(|state| state.cursor.char_range())
            .map(|range| range.primary.index.0)
            .unwrap_or_else(|| self.chat_input.chars().count());
        let chars: Vec<char> = self.chat_input.chars().collect();
        let cursor = cursor.min(chars.len());
        let mut start = cursor;
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        let prefix: String = chars[start..cursor].iter().collect();

        // Siguiente candidato si se vuelve a pulsar Tab.
        if let Some(completion) = &mut self.chat_completion {
            if completion.start == start && !completion.candidates.is_empty() {
                completion.index = (completion.index + 1) % completion.candidates.len();
                let candidate = completion.candidates[completion.index].clone();
                self.replace_range(start, cursor, &candidate);
                return;
            }
        }
        let candidates = self.completion_candidates(&prefix);
        if candidates.is_empty() {
            return;
        }
        let candidate = candidates[0].clone();
        self.replace_range(start, cursor, &candidate);
        self.chat_completion = Some(Completion {
            start,
            candidates,
            index: 0,
        });
    }

    fn completion_candidates(&mut self, prefix: &str) -> Vec<String> {
        if prefix.is_empty() {
            return Vec::new();
        }
        let lower = prefix.to_lowercase();
        if lower.starts_with('/') {
            return [
                "/me ",
                "/w ",
                "/help",
                "/clear",
                "/history",
                "/ping",
                "/uptime",
                "/color ",
                "/announce ",
            ]
            .iter()
            .filter(|command| command.to_lowercase().starts_with(&lower))
            .map(|command| command.to_string())
            .collect();
        }
        let Some(channel) = self.chat_channel.clone() else {
            return Vec::new();
        };
        let mut out: Vec<String> = Vec::new();
        let channel_emotes = self.emotes_for_channel(&channel);
        for entry in channel_emotes.iter() {
            if entry.name.to_lowercase().starts_with(&lower) {
                out.push(entry.name.clone());
            }
        }
        let mut seen: HashSet<String> = HashSet::new();
        for event in self.chat_messages.iter().rev().take(250) {
            if event.channel != channel {
                continue;
            }
            if !event.login.is_empty()
                && event.login.starts_with(&lower)
                && seen.insert(event.login.clone())
            {
                out.push(event.login.clone());
            }
        }
        out.sort();
        out.dedup();
        out.truncate(40);
        out
    }

    fn replace_range(&mut self, start: usize, end: usize, replacement: &str) {
        let chars: Vec<char> = self.chat_input.chars().collect();
        let start = start.min(chars.len());
        let end = end.min(chars.len()).max(start);
        let before: String = chars[..start].iter().collect();
        let after: String = chars[end..].iter().collect();
        self.chat_input = format!("{before}{replacement}{after}");
    }

    fn insert_text(&mut self, text: &str) {
        let lead = if self.chat_input.is_empty() || self.chat_input.ends_with(' ') {
            ""
        } else {
            " "
        };
        self.chat_input.push_str(lead);
        self.chat_input.push_str(text);
    }

    fn insert_emote(&mut self, name: &str) {
        let lead = if self.chat_input.is_empty() || self.chat_input.ends_with(' ') {
            ""
        } else {
            " "
        };
        self.chat_input.push_str(lead);
        self.chat_input.push_str(name);
        self.chat_input.push(' ');
        self.chat_focus_input = true;
    }

    // -- paneles flotantes ---------------------------------------------------

    fn chat_popups(&mut self, ctx: &egui::Context) {
        if self.emote_picker {
            if let Some(anchor) = self.chat_input_rect {
                self.emote_picker_popup(ctx, anchor);
            }
        }
        if self.points_popup_open {
            if let Some(anchor) = self.chat_input_rect {
                self.points_rewards_popup(ctx, anchor);
            }
        }
        if self.chat_settings_open {
            self.chat_settings_popup(ctx);
        }
    }

    fn points_rewards_popup(&mut self, ctx: &egui::Context, anchor: egui::Rect) {
        let p = self.palette();
        let Some(channel) = self.chat_channel.clone() else {
            self.points_popup_open = false;
            return;
        };
        let key = channel.to_lowercase();
        let width = (anchor.width() - 12.0).clamp(280.0, 410.0);
        let top_limit = self
            .chat_panel_rect
            .map(|rect| rect.top() + 6.0)
            .unwrap_or(anchor.top() - 360.0);
        let height = (anchor.top() - top_limit - 6.0).clamp(240.0, 420.0);
        let position = egui::pos2(anchor.left(), anchor.top() - height - 6.0);

        let context = self.mining_contexts.get(&key).cloned();
        let balance = self
            .mining_points
            .get(&key)
            .copied()
            .or_else(|| context.as_ref().and_then(|c| c.balance));
        let watching = self.embedded_channel.as_deref() == Some(key.as_str())
            || self.running.keys().any(|k| k.eq_ignore_ascii_case(&key));
        let watch_ticks = self.mining_watch_ticks.get(&key).copied().unwrap_or(0);

        let mut close_popup = false;
        let mut refresh_clicked = false;
        let mut open_web_clicked = false;
        let mut redeem_action: Option<(String, crate::gql::CustomReward, Option<String>)> = None;

        egui::Area::new(egui::Id::unique("points_rewards_popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(position)
            .constrain(true)
            .show(ctx, |ui| {
                anim::shadow(
                    ui.painter(),
                    egui::Rect::from_min_size(position, egui::vec2(width, height)),
                    12.0,
                    [0, 8],
                    24,
                    egui::Color32::from_black_alpha(90),
                );
                egui::Frame::NONE
                    .fill(p.surface)
                    .stroke(egui::Stroke::new(1.0, p.border2))
                    .corner_radius(egui::CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(12))
                    .show(ui, |ui| {
                        ui.set_width(width - 24.0);
                        ui.set_min_height(height - 24.0);
                        ui.set_max_height(height - 24.0);

                        // Header
                        ui.horizontal(|ui| {
                            let title = context
                                .as_ref()
                                .map(|c| c.points_name.as_str())
                                .unwrap_or("Puntos del canal");
                            ui.label(
                                egui::RichText::new(title)
                                    .size(13.0)
                                    .strong()
                                    .color(p.text),
                            );
                            if let Some(pts) = balance {
                                ui.label(
                                    egui::RichText::new(format!("· {} pts", super::format_count(pts)))
                                        .size(11.5)
                                        .strong()
                                        .color(p.gold),
                                );
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if self
                                    .mini_icon(ui, Icon::Close, "Cerrar", p.muted, &p)
                                    .clicked()
                                {
                                    close_popup = true;
                                }
                                if self
                                    .mini_icon(
                                        ui,
                                        Icon::External,
                                        "Abrir canal en Twitch",
                                        p.muted,
                                        &p,
                                    )
                                    .clicked()
                                {
                                    open_web_clicked = true;
                                }
                                if self
                                    .mini_icon(
                                        ui,
                                        Icon::Refresh,
                                        "Actualizar saldo y recompensas",
                                        p.muted,
                                        &p,
                                    )
                                    .clicked()
                                {
                                    refresh_clicked = true;
                                }
                            });
                        });

                        // Auto-earning status line
                        ui.horizontal(|ui| {
                            let (dot, msg) = if watching && self.mining_running {
                                (
                                    p.green,
                                    if watch_ticks > 0 {
                                        format!("Recibiendo puntos automáticamente ({watch_ticks} min)")
                                    } else {
                                        "Recibiendo puntos automáticamente".to_string()
                                    },
                                )
                            } else if self.mining_connected {
                                (
                                    p.muted,
                                    "Reproduce el directo para sumar puntos por visualización"
                                        .to_string(),
                                )
                            } else {
                                (p.gold, "Conectando servicio de puntos...".to_string())
                            };
                            ui.colored_label(dot, "●");
                            ui.label(egui::RichText::new(msg).size(10.0).color(p.muted));
                        });

                        ui.add_space(6.0);
                        ui.separator();
                        ui.add_space(6.0);

                        // Selected reward text-input form (if any)
                        if let (Some(sel_id), Some(ctx_data)) =
                            (self.points_selected_reward.clone(), context.as_ref())
                        {
                            if let Some(reward) =
                                ctx_data.rewards.iter().find(|r| r.id == sel_id).cloned()
                            {
                                egui::Frame::NONE
                                    .fill(p.surface2)
                                    .stroke(egui::Stroke::new(1.0, p.accent))
                                    .corner_radius(egui::CornerRadius::same(8))
                                    .inner_margin(egui::Margin::same(8))
                                    .show(ui, |ui| {
                                        ui.label(
                                            egui::RichText::new(&reward.title)
                                                .size(11.5)
                                                .strong()
                                                .color(p.text),
                                        );
                                        if !reward.prompt.is_empty() {
                                            ui.label(
                                                egui::RichText::new(&reward.prompt)
                                                    .size(10.0)
                                                    .color(p.muted),
                                            );
                                        }
                                        ui.add_space(4.0);
                                        let edit_res = ui.add(
                                            egui::TextEdit::singleline(
                                                &mut self.points_reward_input,
                                            )
                                            .desired_width(f32::INFINITY)
                                            .hint_text("Escribe el texto para el canje..."),
                                        );
                                        ui.add_space(6.0);
                                        let can_submit = !self.points_redeeming
                                            && !self.points_reward_input.trim().is_empty();
                                        let enter = edit_res.lost_focus()
                                            && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                        ui.horizontal(|ui| {
                                            if ui
                                                .add_enabled(
                                                    can_submit,
                                                    egui::Button::new(
                                                        egui::RichText::new(format!(
                                                            "Canjear ({} pts)",
                                                            super::format_count(reward.cost)
                                                        ))
                                                        .size(11.0)
                                                        .strong()
                                                        .color(p.on_accent),
                                                    )
                                                    .fill(p.accent),
                                                )
                                                .clicked()
                                                || (enter && can_submit)
                                            {
                                                redeem_action = Some((
                                                    ctx_data.channel_id.clone(),
                                                    reward.clone(),
                                                    Some(self.points_reward_input.clone()),
                                                ));
                                            }
                                            if ui.button("Cancelar").clicked() {
                                                self.points_selected_reward = None;
                                                self.points_reward_input.clear();
                                            }
                                        });
                                    });
                                ui.add_space(6.0);
                            }
                        }

                        // Rewards list
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| match context.as_ref() {
                                None => {
                                    ui.horizontal(|ui| {
                                        let (sr, _) = ui.allocate_exact_size(
                                            egui::vec2(14.0, 14.0),
                                            egui::Sense::hover(),
                                        );
                                        super::spinner(
                                            ui.painter(),
                                            sr.center(),
                                            6.0,
                                            ui.input(|i| i.time),
                                            p.accent,
                                        );
                                        ui.label(
                                            egui::RichText::new("Cargando recompensas...")
                                                .size(11.0)
                                                .color(p.muted),
                                        );
                                    });
                                }
                                Some(ctx_data) if !ctx_data.is_enabled => {
                                    ui.label(
                                        egui::RichText::new(
                                            "Este canal no tiene los puntos del canal activados.",
                                        )
                                        .size(11.0)
                                        .color(p.muted),
                                    );
                                }
                                Some(ctx_data) if ctx_data.rewards.is_empty() => {
                                    ui.label(
                                        egui::RichText::new(
                                            "Este canal no tiene recompensas personalizadas disponibles.",
                                        )
                                        .size(11.0)
                                        .color(p.muted),
                                    );
                                }
                                Some(ctx_data) => {
                                    let current_pts = balance.unwrap_or(0);
                                    for reward in &ctx_data.rewards {
                                        let affordable =
                                            balance.map(|b| b >= reward.cost).unwrap_or(true);
                                        let on_cooldown = reward.cooldown_expires_at.is_some();
                                        let available = !reward.is_paused
                                            && reward.is_in_stock
                                            && !on_cooldown
                                            && affordable
                                            && !self.points_redeeming;
                                        let swatch = super::parse_hex_color(&reward.background_color)
                                            .unwrap_or(p.accent);

                                        let frame = egui::Frame::NONE
                                            .fill(if available { p.surface2 } else { p.surface })
                                            .stroke(egui::Stroke::new(
                                                1.0,
                                                if available { p.border2 } else { p.border },
                                            ))
                                            .corner_radius(egui::CornerRadius::same(8))
                                            .inner_margin(egui::Margin::symmetric(10, 7))
                                            .show(ui, |ui| {
                                                ui.horizontal(|ui| {
                                                    let (dot_rect, _) = ui.allocate_exact_size(
                                                        egui::vec2(8.0, 26.0),
                                                        egui::Sense::hover(),
                                                    );
                                                    ui.painter().rect_filled(
                                                        dot_rect,
                                                        egui::CornerRadius::same(4),
                                                        if available {
                                                            swatch
                                                        } else {
                                                            theme::with_alpha(swatch, 0.35)
                                                        },
                                                    );
                                                    ui.vertical(|ui| {
                                                        ui.spacing_mut().item_spacing.y = 1.0;
                                                        ui.label(
                                                            egui::RichText::new(&reward.title)
                                                                .size(11.5)
                                                                .strong()
                                                                .color(if available {
                                                                    p.text
                                                                } else {
                                                                    p.dim
                                                                }),
                                                        );
                                                        let mut meta = Vec::new();
                                                        if reward.is_paused {
                                                            meta.push("Pausado");
                                                        }
                                                        if !reward.is_in_stock {
                                                            meta.push("Sin stock");
                                                        }
                                                        if on_cooldown {
                                                            meta.push("Enfriamiento");
                                                        }
                                                        if reward.is_user_input_required {
                                                            meta.push("Requiere texto");
                                                        }
                                                        if !affordable {
                                                            meta.push("Saldo insuficiente");
                                                        }
                                                        if !meta.is_empty() {
                                                            ui.label(
                                                                egui::RichText::new(
                                                                    meta.join(" · "),
                                                                )
                                                                .size(9.5)
                                                                .color(p.dim),
                                                            );
                                                        } else if !reward.prompt.is_empty() {
                                                            ui.label(
                                                                egui::RichText::new(
                                                                    super::ellipsize(
                                                                        &reward.prompt,
                                                                        48,
                                                                    ),
                                                                )
                                                                .size(9.5)
                                                                .color(p.muted),
                                                            );
                                                        }
                                                    });
                                                    ui.with_layout(
                                                        egui::Layout::right_to_left(
                                                            egui::Align::Center,
                                                        ),
                                                        |ui| {
                                                            let cost_btn = egui::Button::new(
                                                                egui::RichText::new(
                                                                    super::format_count(
                                                                        reward.cost,
                                                                    ),
                                                                )
                                                                .size(10.5)
                                                                .strong()
                                                                .color(if available {
                                                                    p.on_accent
                                                                } else {
                                                                    p.dim
                                                                }),
                                                            )
                                                            .fill(if available {
                                                                p.accent
                                                            } else {
                                                                p.surface3
                                                            })
                                                            .corner_radius(
                                                                egui::CornerRadius::same(10),
                                                            );
                                                            if ui
                                                                .add_enabled(available, cost_btn)
                                                                .clicked()
                                                            {
                                                                if reward.is_user_input_required {
                                                                    self.points_selected_reward =
                                                                        Some(reward.id.clone());
                                                                    self.points_reward_input
                                                                        .clear();
                                                                } else {
                                                                    redeem_action = Some((
                                                                        ctx_data.channel_id.clone(),
                                                                        reward.clone(),
                                                                        None,
                                                                    ));
                                                                }
                                                            }
                                                        },
                                                    );
                                                });
                                            });
                                        let _ = (frame, current_pts);
                                        ui.add_space(5.0);
                                    }
                                }
                            });
                    });
            });

        if close_popup || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.points_popup_open = false;
        }
        if refresh_clicked {
            self.mining_refresh_channel(&key);
        }
        if open_web_clicked {
            open_external(&format!("https://www.twitch.tv/{key}"));
        }
        if let Some((channel_id, reward, text_input)) = redeem_action {
            if channel_id.is_empty() {
                self.set_toast("Falta el ID del canal para canjear la recompensa");
            } else {
                self.points_redeeming = true;
                self.ensure_mining_started();
                let _ = self.mining_tx.send(crate::mining::Command::Redeem {
                    channel: key,
                    channel_id,
                    reward,
                    text_input,
                });
            }
        }
    }

    fn emote_picker_popup(&mut self, ctx: &egui::Context, anchor: egui::Rect) {
        let p = self.palette();
        let width = (anchor.width() - 16.0).clamp(290.0, 430.0);
        // Nunca debe salirse del panel: se limita al hueco entre la barra
        // superior del chat y el campo de escritura.
        let top_limit = self
            .chat_panel_rect
            .map(|rect| rect.top() + 6.0)
            .unwrap_or(anchor.top() - 340.0);
        let height = (anchor.top() - top_limit - 6.0).clamp(240.0, 400.0);
        let position = egui::pos2(anchor.right() - width, anchor.top() - height - 6.0);
        egui::Area::new(egui::Id::unique("emote_picker_popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(position)
            .constrain(true)
            .show(ctx, |ui| {
                // Sombra suave bajo el panel flotante (la referencia la usa
                // para separar el selector del chat).
                anim::shadow(
                    ui.painter(),
                    egui::Rect::from_min_size(position, egui::vec2(width, height)),
                    12.0,
                    [0, 6],
                    20,
                    egui::Color32::from_black_alpha(46),
                );
                egui::Frame::NONE
                    .fill(p.surface)
                    .stroke(egui::Stroke::new(1.0, p.border2))
                    .corner_radius(egui::CornerRadius::same(10))
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.set_width(width - 20.0);
                        ui.set_height(height - 16.0);
                        self.emote_picker_contents(ui, height - 16.0);
                    });
            });
    }

    fn emote_picker_contents(&mut self, ui: &mut egui::Ui, total_height: f32) {
        let p = self.palette();
        let cache_dir = self.chat_channel.as_deref().map(emote_cache::channel_dir);
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
            icons::paint(ui.painter(), rect, Icon::SevenTv, p.accent, p.surface);
            ui.label(
                egui::RichText::new("Emotes")
                    .size(12.0)
                    .strong()
                    .color(p.text),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.mini_icon(ui, Icon::Close, "Cerrar", p.muted, &p).clicked() {
                    self.emote_picker = false;
                }
                let count = self.emote_count();
                ui.label(
                    egui::RichText::new(format!("{count}"))
                        .size(9.5)
                        .color(p.dim),
                );
            });
        });
        ui.add_space(4.0);
        ui.add_sized(
            [ui.available_width(), 24.0],
            egui::TextEdit::singleline(&mut self.emote_search)
                .hint_text("Buscar emote...")
                .text_color(p.text),
        );
        ui.add_space(4.0);
        let selected = match self.emote_source {
            EmoteSource::All => 0,
            EmoteSource::SevenTv => 1,
            EmoteSource::Twitch => 2,
            EmoteSource::Bttv => 3,
            EmoteSource::Ffz => 4,
        };
        if let Some(index) = self.segmented_index(
            ui,
            "emote_source_tabs",
            &["Todos", "7TV", "Twitch", "BTTV", "FFZ"],
            selected,
            &p,
            24.0,
        ) {
            self.emote_source = [
                EmoteSource::All,
                EmoteSource::SevenTv,
                EmoteSource::Twitch,
                EmoteSource::Bttv,
                EmoteSource::Ffz,
            ][index];
        }
        ui.add_space(4.0);

        let Some(channel) = self.chat_channel.clone() else {
            return;
        };
        let filter = self.emote_search.to_lowercase();
        let source = self.emote_source;
        // Reconstruir el filtro solo cuando cambia algo. Antes se recorrían y
        // clonaban 1500 emotes en cada frame.
        if self.emote_picker_for != channel
            || self.emote_picker_source != source
            || self.emote_picker_query != filter
            || self.emote_picker_generation != self.emote_generation
        {
            let list = self.emotes_for_channel(&channel);
            self.emote_picker_items = list
                .iter()
                .filter(|entry| {
                    let source_ok = match source {
                        EmoteSource::All => true,
                        EmoteSource::SevenTv => entry.origin == EmoteOrigin::SevenTv,
                        EmoteSource::Twitch => entry.origin == EmoteOrigin::Twitch,
                        EmoteSource::Bttv => entry.origin == EmoteOrigin::Bttv,
                        EmoteSource::Ffz => entry.origin == EmoteOrigin::Ffz,
                    };
                    source_ok
                        && (filter.is_empty() || entry.name.to_lowercase().contains(&filter))
                })
                .cloned()
                .collect();
            self.emote_picker_for = channel.clone();
            self.emote_picker_source = source;
            self.emote_picker_query = filter;
            self.emote_picker_generation = self.emote_generation;
        }
        if self.emote_picker_items.is_empty() {
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new("Sin emotes para este filtro")
                    .size(10.5)
                    .color(p.dim),
            );
            return;
        }
        // Se saca la lista del estado para poder usar `self.images` dentro del
        // cierre sin préstamos enfrentados. Se devuelve al terminar.
        let emotes = std::mem::take(&mut self.emote_picker_items);
        let mut chosen: Option<String> = None;
        let hint = 18.0;
        let grid_height = (total_height - 112.0 - hint).max(80.0);
        // Agrupar por (origen, canal dueño): los emotes de suscripción de
        // otros canales llevan su propio grupo con el icono del canal.
        let mut groups: Vec<((EmoteOrigin, String), Vec<&EmoteEntry>)> = Vec::new();
        for entry in &emotes {
            let key = (entry.origin, entry.owner.clone());
            match groups.last_mut() {
                Some((last_key, items)) if *last_key == key => items.push(entry),
                _ => groups.push((key, vec![entry])),
            }
        }
        egui::ScrollArea::vertical()
            .id_salt("emote_picker_scroll")
            .max_height(grid_height)
            .min_scrolled_height(grid_height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                for ((origin, owner), items) in &groups {
                    // Cabecera del grupo: icono del canal + origen.
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        if !owner.is_empty() {
                            self.avatar(ui, owner, 14.0);
                            ui.label(
                                egui::RichText::new(format!("#{owner}"))
                                    .size(9.5)
                                    .strong()
                                    .color(p.muted),
                            );
                        }
                        ui.label(
                            egui::RichText::new(origin.label())
                                .monospace()
                                .size(8.5)
                                .color(p.dim),
                        );
                    });
                    ui.horizontal_wrapped(|ui| {
                        for entry in items {
                            let name = &entry.name;
                            let url = &entry.url;
                            let (rect, response) = ui
                                .allocate_exact_size(egui::vec2(32.0, 32.0), egui::Sense::click());
                            let hovered = response.hovered();
                            let visible = ui.clip_rect().intersects(rect);
                            if visible {
                                ui.painter().rect_filled(
                                    rect,
                                    egui::CornerRadius::same(6),
                                    if hovered {
                                        p.surface3
                                    } else {
                                        theme::with_alpha(p.surface2, 0.55)
                                    },
                                );
                            }
                            // Solo se descargan los emotes visibles (el resto
                            // llegará al desplazarse): ahorra memoria y red.
                            // `is_rect_visible` mira el viewport entero; para el
                            // ScrollArea hay que usar el rectángulo recortado o
                            // se acaban decodificando las celdas de golpe.
                            if visible {
                                let inner = rect.shrink(3.0);
                                if let Some(texture) = self.images.get_cached(
                                    ui.ctx(),
                                    url,
                                    32,
                                    32,
                                    cache_dir.as_deref(),
                                ) {
                                    ui.painter().image(
                                        texture.id(),
                                        inner,
                                        egui::Rect::from_min_max(
                                            egui::pos2(0.0, 0.0),
                                            egui::pos2(1.0, 1.0),
                                        ),
                                        egui::Color32::WHITE,
                                    );
                                } else {
                                    ui.painter().text(
                                        rect.center(),
                                        egui::Align2::CENTER_CENTER,
                                        name.chars().next().unwrap_or('?').to_uppercase().to_string(),
                                        egui::FontId::new(11.0, egui::FontFamily::Proportional),
                                        p.dim,
                                    );
                                }
                            }
                            // El tooltip solo se construye para la celda bajo el
                            // cursor, no para todas en cada frame.
                            let response = if hovered {
                                let origin_label = if entry.owner.is_empty() {
                                    origin.label().to_string()
                                } else {
                                    format!("{} · #{}", origin.label(), entry.owner)
                                };
                                response.on_hover_text(format!("{name}\n{origin_label}"))
                            } else {
                                response
                            };
                            if response.clicked() {
                                chosen = Some(name.clone());
                            }
                        }
                    });
                    ui.add_space(2.0);
                }
            });
        self.emote_picker_items = emotes;
        if let Some(name) = chosen {
            self.insert_emote(&name);
        }
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new("Tab completa el nombre · clic inserta el emote")
                .size(9.0)
                .color(p.dim),
        );
    }

    fn emote_count(&mut self) -> usize {
        self.chat_channel
            .clone()
            .map(|channel| self.emotes_for_channel(&channel).len())
            .unwrap_or(0)
    }

    fn chat_settings_popup(&mut self, ctx: &egui::Context) {
        let p = self.palette();
        let screen = ctx.viewport_rect();
        egui::Area::new(egui::Id::unique("chat_settings_popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(screen.right() - 300.0, 76.0))
            .constrain(true)
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(p.surface)
                    .stroke(egui::Stroke::new(1.0, p.border2))
                    .corner_radius(egui::CornerRadius::same(10))
                    .inner_margin(egui::Margin::symmetric(12, 10))
                    .show(ui, |ui| {
                        ui.set_width(260.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("Ajustes del chat")
                                    .size(12.0)
                                    .strong()
                                    .color(p.text),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if self
                                        .mini_icon(ui, Icon::Close, "Cerrar", p.muted, &p)
                                        .clicked()
                                    {
                                        self.chat_settings_open = false;
                                        let _ = self.cfg.save();
                                    }
                                },
                            );
                        });
                        ui.add_space(6.0);
                        ui.add(
                            egui::Slider::new(&mut self.cfg.chat_width, 320.0..=620.0)
                                .text("Ancho")
                                .suffix(" px"),
                        );
                        ui.add(
                            egui::Slider::new(&mut self.cfg.chat_font, 10.0..=18.0)
                                .text("Letra")
                                .step_by(0.5),
                        );
                        ui.checkbox(&mut self.cfg.chat_badges, "Mostrar insignias");
                        ui.checkbox(&mut self.cfg.chat_time, "Mostrar la hora");
                        ui.checkbox(&mut self.cfg.chat_alt, "Filas alternas");
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            if ui.button("Recargar historial").clicked() {
                                if let Some(channel) = self.chat_channel.clone() {
                                    self.chat_history_requested.remove(&channel);
                                    self.request_history(&channel);
                                    self.chat_follow = true;
                                }
                            }
                            if ui.button("Limpiar").clicked() {
                                if let Some(channel) = self.chat_channel.clone() {
                                    self.chat_messages
                                        .retain(|event| event.channel != channel);
                                }
                            }
                        });
                        ui.add_space(6.0);
                        ui.separator();
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            if ui.button("Chat web").clicked() {
                                self.chat_open_web();
                                self.chat_settings_open = false;
                            }
                            if ui.button("Chatterino").clicked() {
                                self.chat_open_chatterino();
                                self.chat_settings_open = false;
                            }
                        });
                        ui.add_space(4.0);
                        if ui.button("Cerrar chat y liberar memoria").clicked() {
                            self.chat_settings_open = false;
                            self.chat_close();
                        }
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(
                                "El historial se descarga de recent-messages, igual que en Chatterino.",
                            )
                            .size(9.0)
                            .color(p.dim),
                        );
                    });
            });
    }

    // -- eventos y estadísticas ---------------------------------------------

    fn chat_events_view(&mut self, ui: &mut egui::Ui) {
        let p = self.palette();
        let events: Vec<chat::ChatEvent> = self
            .chat_messages
            .iter()
            .filter(|event| {
                !matches!(
                    event.kind,
                    chat::Kind::Message | chat::Kind::Action | chat::Kind::Status
                )
            })
            .rev()
            .take(80)
            .cloned()
            .collect();
        egui::ScrollArea::vertical()
            .id_salt("chat_events")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if events.is_empty() {
                    ui.add_space(14.0);
                    ui.label(
                        egui::RichText::new("Sin eventos todavía")
                            .size(11.0)
                            .color(p.dim),
                    );
                    return;
                }
                for event in events {
                    let (icon, color) = match event.kind {
                        chat::Kind::System => (Icon::Gift, p.gold),
                        chat::Kind::Whisper => (Icon::Chat, p.gold),
                        chat::Kind::Delete | chat::Kind::Clear => (Icon::Trash, p.red),
                        _ => (Icon::Bolt, p.dim),
                    };
                    egui::Frame::NONE
                        .fill(theme::with_alpha(p.surface2, 0.7))
                        .corner_radius(egui::CornerRadius::same(7))
                        .inner_margin(egui::Margin::symmetric(8, 5))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(12.0, 12.0),
                                    egui::Sense::hover(),
                                );
                                icons::paint(ui.painter(), rect, icon, color, p.surface2);
                                ui.label(
                                    egui::RichText::new(format!("#{}", event.channel))
                                        .size(9.5)
                                        .color(p.dim),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new(format_time(event.at))
                                                .size(9.0)
                                                .color(p.dim),
                                        );
                                    },
                                );
                            });
                            ui.label(
                                egui::RichText::new(if event.display.is_empty() {
                                    event.text.clone()
                                } else {
                                    format!("{} {}", event.display, event.text)
                                })
                                .size(10.5)
                                .color(p.text),
                            );
                        });
                    ui.add_space(3.0);
                }
            });
    }

    fn chat_stats_view(&mut self, ui: &mut egui::Ui) {
        let p = self.palette();
        let mut channels: Vec<(String, ChatStats)> = self
            .chat_stats
            .iter()
            .map(|(channel, stats)| (channel.clone(), *stats))
            .collect();
        channels.sort_by_key(|entry| std::cmp::Reverse(entry.1.messages));
        egui::ScrollArea::vertical()
            .id_salt("chat_stats")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if channels.is_empty() {
                    ui.label(
                        egui::RichText::new("Sin estadísticas todavía")
                            .size(11.0)
                            .color(p.dim),
                    );
                    return;
                }
                for (channel, stats) in channels {
                    egui::Frame::NONE
                        .fill(theme::with_alpha(p.surface2, 0.7))
                        .corner_radius(egui::CornerRadius::same(7))
                        .inner_margin(egui::Margin::symmetric(8, 6))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                self.avatar(ui, &channel, 16.0);
                                ui.label(
                                    egui::RichText::new(format!("#{channel}"))
                                        .size(11.0)
                                        .strong()
                                        .color(p.text),
                                );
                            });
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing.x = 8.0;
                                for (label, value, color) in [
                                    ("mensajes", stats.messages, p.text),
                                    ("subs", stats.subs, p.accent),
                                    ("regalos", stats.gifts, p.gold),
                                    ("raids", stats.raids, p.green),
                                    ("bits", stats.bits, p.blue),
                                ] {
                                    ui.label(
                                        egui::RichText::new(format!("{value} {label}"))
                                            .size(9.5)
                                            .color(color),
                                    );
                                }
                            });
                        });
                    ui.add_space(4.0);
                }
            });
    }

    /// Icono pequeño (16×16) para las barras del chat.
    fn mini_icon(
        &self,
        ui: &mut egui::Ui,
        icon: Icon,
        tooltip: &str,
        color: egui::Color32,
        p: &Palette,
    ) -> egui::Response {
        let (rect, response) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::click());
        let hover = response.hovered();
        if hover {
            ui.painter().rect_filled(
                rect.expand(2.0),
                egui::CornerRadius::same(4),
                p.surface3,
            );
        }
        let color = if hover { p.text } else { color };
        icons::paint(ui.painter(), rect, icon, color, p.bg);
        response.on_hover_text(tooltip)
    }
}

/// Etiqueta y color de una insignia de Twitch.
fn badge_style(badge: &str, p: &Palette) -> (String, egui::Color32) {
    let (name, version) = badge.split_once('/').unwrap_or((badge, "1"));
    match name {
        "broadcaster" => ("LIVE".to_string(), p.red),
        "moderator" => ("MOD".to_string(), p.green),
        "vip" => ("VIP".to_string(), p.accent),
        "subscriber" => ("SUB".to_string(), p.accent),
        "founder" => ("FOUND".to_string(), p.gold),
        "partner" => ("VERIFIED".to_string(), p.accent),
        "premium" => ("PRIME".to_string(), p.blue),
        "turbo" => ("TURBO".to_string(), p.accent),
        "staff" | "admin" | "global_mod" => ("STAFF".to_string(), p.green),
        "bits" => ("BITS".to_string(), p.gold),
        "sub-gifter" => ("GIFT".to_string(), p.gold),
        "whisper" => ("PRIVADO".to_string(), p.gold),
        "predictions" => ("PRED".to_string(), p.blue),
        "no_audio" => ("SIN AUDIO".to_string(), p.red),
        "no_video" => ("SIN VÍDEO".to_string(), p.red),
        _ => {
            let label = if version.is_empty() {
                name.to_uppercase()
            } else {
                name.chars().take(4).collect::<String>().to_uppercase()
            };
            (label, p.muted)
        }
    }
}

/// Adjusts Twitch username colors so they remain legible on both light and dark Cozy themes.
fn readable_chat_color(color: egui::Color32, p: &Palette) -> egui::Color32 {
    let lum = (0.2126 * color.r() as f32 + 0.7152 * color.g() as f32 + 0.0722 * color.b() as f32)
        / 255.0;
    let dark_bg = theme::is_color_dark(p.surface);
    if !dark_bg && lum > 0.62 {
        // Darken overly bright pastel/neon colors on light themes (Cozy Milk).
        theme::lerp_color(color, p.text, 0.38)
    } else if dark_bg && lum < 0.34 {
        // Lighten overly dark colors on dark themes (Cozy Café).
        theme::lerp_color(color, p.text, 0.45)
    } else {
        color
    }
}

/// `contains` sin distinguir mayúsculas y sin reservar memoria (las menciones
/// se comprobaban con `to_lowercase()` en cada fila y en cada frame).
fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let hay = haystack.as_bytes();
    let needle = needle.as_bytes();
    needle.len() <= hay.len() && hay.windows(needle.len()).any(|part| part.eq_ignore_ascii_case(needle))
}

fn format_time(at: std::time::SystemTime) -> String {
    at.duration_since(std::time::UNIX_EPOCH)
        .map(|value| {
            let secs = value.as_secs() % 86_400;
            format!("{:02}:{:02}", secs / 3600, (secs % 3600) / 60)
        })
        .unwrap_or_default()
}

fn open_external(url: &str) {
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
}
