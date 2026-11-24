use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use eframe::egui::{self, Color32, Pos2, Rect, Response, Stroke, Ui};
use image::RgbaImage;

use crate::{
    app::Job,
    emoji_catalog::CATEGORIES,
    online::{self, Online},
    overlay::{Content, Layer, Overlays, LAYER_LIMIT, TEXT_LIMIT},
    theme,
};

pub const EMOJI_FONT: &str = "emoji_studio360";
const DEMO_TIMEOUT: Duration = Duration::from_secs(12);
const MAX_STICKER_LOADS: usize = 4;

struct PendingSticker {
    name: &'static str,
    emoji: &'static str,
    position: [f32; 2],
    job: Option<Job<Result<RgbaImage, online::Error>>>,
}

#[derive(Default)]
pub struct OverlayEditor {
    layers: Overlays,
    revision: u64,
    textures: Vec<Option<(Arc<RgbaImage>, egui::TextureHandle)>>,
    selected: Option<usize>,
    moving: bool,
    category: usize,
    search: String,
    pending: Vec<PendingSticker>,
    error: Option<String>,
    demo: Option<Instant>,
}

impl OverlayEditor {
    pub fn snapshot(&self) -> &Overlays {
        &self.layers
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn edited(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn demo_ready(&self) -> bool {
        self.demo.is_none()
    }

    pub fn start_demo(&mut self, ctx: &egui::Context, online: &Online) {
        self.demo = Some(Instant::now());
        self.load(ctx, online, "Occhiali da sole", "😎", [0.43, 0.48]);
        self.load(ctx, online, "Scintille", "✨", [0.58, 0.57]);
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        let mut index = 0;
        while index < self.pending.len() {
            let result = Job::take(&mut self.pending[index].job);
            if self.pending[index].job.is_some() {
                index += 1;
                continue;
            }
            let pending = self.pending.remove(index);
            match result {
                Some(Ok(image)) => {
                    let mut layer = Layer::sticker(pending.name, image);
                    layer.center = pending.position;
                    let already_added = self.selected.is_some();
                    self.push(layer);
                    if self.demo.is_some() && !already_added {
                        self.selected = None;
                    }
                }
                Some(Err(error)) => self.error = Some(format!("{} · {error}", pending.name)),
                None => self.error = Some("Download dello sticker interrotto.".into()),
            }
        }
        if let Some(since) = self.demo {
            if self.pending.is_empty() {
                self.demo = None;
            } else if since.elapsed() >= DEMO_TIMEOUT {
                self.pending.clear();
                self.error = Some(
                    "Errore di rete: controlla la connessione. Attesa degli sticker scaduta."
                        .into(),
                );
                self.demo = None;
            } else {
                ctx.request_repaint_after(DEMO_TIMEOUT.saturating_sub(since.elapsed()));
            }
        }
    }

    fn push(&mut self, layer: Layer) {
        self.selected = Some(self.layers.layers.len());
        self.layers.layers.push(layer);
        self.textures.push(None);
        self.edited();
    }

    fn load(
        &mut self,
        ctx: &egui::Context,
        online: &Online,
        name: &'static str,
        emoji: &'static str,
        position: [f32; 2],
    ) {
        if let Some(image) = self.layers.layers.iter().find_map(|layer| {
            matches!(layer.content, Content::Sticker(label) if label == name)
                .then(|| Arc::clone(&layer.image))
        }) {
            let mut layer = Layer::sticker(name, image);
            layer.center = position;
            self.push(layer);
            return;
        }
        let online = online.clone();
        self.error = None;
        self.pending.push(PendingSticker {
            name,
            emoji,
            position,
            job: Some(Job::start(ctx, move || online.emoji(emoji))),
        });
    }

    pub fn stickers(&mut self, ui: &mut Ui, online: &Online, duration: Option<f64>) {
        theme::heading(ui, "Sticker");
        ui.hyperlink_to(online::TWEMOJI_CREDIT, online::TWEMOJI_LINK);
        if !online.enabled() {
            theme::muted(ui, "Offline · puoi usare gli sticker in cache.");
        }
        let label = if self.layers.show_stickers {
            "Nascondi sticker"
        } else {
            "Mostra sticker"
        };
        if ui.button(label).clicked() {
            self.layers.show_stickers = !self.layers.show_stickers;
            self.edited();
        }
        ui.add_space(6.0);
        ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .hint_text("Cerca uno sticker")
                .char_limit(60)
                .desired_width(f32::INFINITY),
        );
        let query = self.search.trim().to_lowercase();
        if query.is_empty() {
            ui.horizontal_wrapped(|ui| {
                for (i, category) in CATEGORIES.iter().enumerate() {
                    if theme::chip(ui, category.name, self.category == i).clicked() {
                        self.category = i;
                    }
                }
            });
        }
        let mut found = Vec::new();
        for (i, category) in CATEGORIES.iter().enumerate() {
            if i == self.category || !query.is_empty() {
                found.extend(
                    category
                        .emojis
                        .iter()
                        .copied()
                        .filter(|(name, _)| name.to_lowercase().contains(&query)),
                );
            }
        }
        if found.is_empty() {
            theme::muted(ui, "Nessun risultato");
        }
        let mut add = None;
        const COLUMNS: usize = 4;
        let gap = ui.spacing().item_spacing.x;
        let cell_width = ((ui.available_width() - 3.0 * gap - 12.0) / COLUMNS as f32)
            .floor()
            .max(1.0);
        let font = egui::FontId::new(20.0, egui::FontFamily::Name(EMOJI_FONT.into()));
        egui::ScrollArea::vertical()
            .id_salt("griglia_sticker")
            .max_height(190.0)
            .show(ui, |ui| {
                for row in found.chunks(COLUMNS) {
                    ui.horizontal(|ui| {
                        for &(name, emoji) in row {
                            let waiting = self.pending.iter().any(|pending| pending.emoji == emoji);
                            let allowed = !waiting
                                && self.pending.len() < MAX_STICKER_LOADS
                                && self.layers.layers.len() + self.pending.len() < LAYER_LIMIT;
                            let glyph = emoji.trim_end_matches('\u{fe0f}');
                            let text = if ui.fonts_mut(|fonts| fonts.has_glyphs(&font, glyph)) {
                                egui::RichText::new(glyph).font(font.clone())
                            } else {
                                egui::RichText::new(name).small()
                            };
                            let response = ui
                                .add_enabled_ui(allowed, |ui| {
                                    ui.add_sized(
                                        [cell_width, 33.0],
                                        egui::Button::new(text).truncate(),
                                    )
                                })
                                .inner
                                .on_hover_text(name);
                            response.widget_info(|| {
                                egui::WidgetInfo::labeled(egui::Role::Button, allowed, name)
                            });
                            if response.clicked() {
                                add = Some((name, emoji));
                            }
                        }
                    });
                }
            });
        if let Some((name, emoji)) = add {
            if !self.layers.show_stickers {
                self.layers.show_stickers = true;
                self.edited();
            }
            self.load(ui.ctx(), online, name, emoji, [0.5, 0.5]);
        }
        if !self.pending.is_empty() {
            let names = self
                .pending
                .iter()
                .map(|pending| pending.name)
                .collect::<Vec<_>>()
                .join(", ");
            theme::muted(ui, format!("Download… {names}"));
        }
        if let Some(error) = &self.error {
            ui.colored_label(theme::ERROR, error);
        }
        self.controls(ui, true, duration);
    }

    pub fn text(&mut self, ui: &mut Ui, duration: Option<f64>) {
        theme::heading(ui, "Testo");
        let can_add = self.layers.layers.len() + self.pending.len() < LAYER_LIMIT;
        if ui
            .add_enabled(can_add, theme::primary("Aggiungi testo"))
            .clicked()
        {
            self.push(Layer::text());
        }
        self.controls(ui, false, duration);
    }

    fn controls(&mut self, ui: &mut Ui, sticker: bool, duration: Option<f64>) {
        let matches = |layer: &Layer| layer.is_sticker() == sticker;
        if !self.layers.layers.iter().any(matches) {
            return;
        }
        ui.separator();
        if self
            .selected
            .is_none_or(|i| !matches(&self.layers.layers[i]))
        {
            self.selected = self.layers.layers.iter().rposition(matches);
        }
        let Some(mut index) = self.selected else {
            return;
        };
        egui::ComboBox::from_id_salt("livello_attivo")
            .width(ui.available_width() - 10.0)
            .selected_text(short_label(self.layers.layers[index].label()))
            .show_ui(ui, |ui| {
                for (i, layer) in self
                    .layers
                    .layers
                    .iter()
                    .enumerate()
                    .filter(|(_, layer)| matches(layer))
                {
                    ui.selectable_value(&mut index, i, short_label(layer.label()));
                }
            });
        self.selected = Some(index);
        let layer = &mut self.layers.layers[index];
        let mut changed = false;
        if let Content::Text(style) = &mut layer.content {
            let mut refresh = ui
                .add(
                    egui::TextEdit::multiline(&mut style.value)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY)
                        .char_limit(TEXT_LIMIT)
                        .hint_text("Scrivi il testo"),
                )
                .changed();
            ui.horizontal(|ui| {
                refresh |= ui
                    .color_edit_button_srgba_unmultiplied(&mut style.fill)
                    .changed();
                ui.label("Colore");
                refresh |= ui
                    .color_edit_button_srgba_unmultiplied(&mut style.outline)
                    .changed();
                ui.label("Contorno");
            });
            refresh |= ui
                .add(egui::Slider::new(&mut style.thickness, 0..=8).text("Spessore"))
                .changed();
            if refresh {
                layer.refresh_text();
                changed = true;
            }
        }
        let mut percent = layer.size * 100.0;
        let bounds = if sticker { 3.0..=85.0 } else { 2.0..=36.0 };
        if ui
            .add(
                egui::Slider::new(&mut percent, bounds)
                    .suffix("%")
                    .text("Dimensione"),
            )
            .changed()
        {
            layer.size = percent / 100.0;
            changed = true;
        }
        if sticker {
            changed |= ui
                .add(
                    egui::Slider::new(&mut layer.angle, -180.0..=180.0)
                        .step_by(1.0)
                        .suffix("°")
                        .text("Rotazione"),
                )
                .changed();
        }
        if let Some(duration) = duration.filter(|&duration| duration > 0.0) {
            let mut timed = layer.start > 0.0 || layer.end.is_some();
            if ui
                .checkbox(&mut timed, "Intervallo personalizzato")
                .changed()
            {
                layer.start = 0.0;
                layer.end = timed.then_some(duration);
                changed = true;
            }
            if timed {
                let mut end = layer.end.unwrap_or(duration).min(duration);
                changed |= ui
                    .add(
                        egui::Slider::new(&mut layer.start, 0.0..=end)
                            .suffix(" s")
                            .text("Da"),
                    )
                    .changed();
                if ui
                    .add(
                        egui::Slider::new(&mut end, layer.start..=duration)
                            .suffix(" s")
                            .text("A"),
                    )
                    .changed()
                {
                    layer.end = Some(end);
                    changed = true;
                }
            }
        }
        if ui
            .button(if sticker {
                "Elimina sticker"
            } else {
                "Elimina testo"
            })
            .clicked()
        {
            self.layers.layers.remove(index);
            self.textures.remove(index);
            self.selected = None;
            changed = true;
        }
        if changed {
            self.edited();
        }
    }

    pub fn handle_input(
        &mut self,
        ui: &Ui,
        response: &Response,
        rect: Rect,
        time: f64,
        sticker: bool,
    ) -> bool {
        let hit = |position: Pos2| {
            if !rect.contains(position) {
                return None;
            }
            let p = position - rect.min;
            self.layers
                .layers
                .iter()
                .enumerate()
                .rfind(|(_, layer)| {
                    layer.is_sticker() == sticker
                        && self.layers.visible(layer, time)
                        && layer
                            .geometry(rect.width(), rect.height())
                            .uv(p.x, p.y)
                            .into_iter()
                            .all(|v| (0.0..=1.0).contains(&v))
                })
                .map(|(i, _)| i)
        };
        if response.drag_started() || response.clicked() {
            let pointer = if response.drag_started() {
                ui.input(|input| input.pointer.press_origin())
            } else {
                response.interact_pointer_pos()
            };
            if let Some(index) = pointer.and_then(hit) {
                self.selected = Some(index);
                self.moving = response.drag_started();
            }
        }
        let hovered = ui.input(|input| input.pointer.hover_pos()).and_then(hit);
        if !response.dragged() {
            self.moving = false;
        }
        if self.moving {
            if let Some(layer) = self.selected.and_then(|i| self.layers.layers.get_mut(i)) {
                let delta = response.drag_delta();
                if delta != egui::Vec2::ZERO {
                    layer.center[0] = (layer.center[0] + delta.x / rect.width()).clamp(0.0, 1.0);
                    layer.center[1] = (layer.center[1] + delta.y / rect.height()).clamp(0.0, 1.0);
                    self.edited();
                }
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                return true;
            }
        }
        if let Some(index) = hovered.filter(|_| response.hovered()) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
            let scroll = ui.input(|input| input.smooth_scroll_delta.y);
            if sticker && scroll != 0.0 {
                self.selected = Some(index);
                let layer = &mut self.layers.layers[index];
                layer.size = (layer.size * (scroll * 0.004).exp()).clamp(0.03, 0.85);
                self.edited();
            }
            return !response.dragged();
        }
        false
    }

    pub fn paint(&mut self, ui: &Ui, rect: Rect, time: f64, editing: Option<bool>) {
        let painter = ui.painter().with_clip_rect(rect);
        let uv = [
            egui::pos2(0.0, 0.0),
            egui::pos2(1.0, 0.0),
            egui::pos2(1.0, 1.0),
            egui::pos2(0.0, 1.0),
        ];
        for (i, layer) in self.layers.layers.iter().enumerate() {
            if !self.layers.visible(layer, time) {
                continue;
            }
            let slot = &mut self.textures[i];
            if slot
                .as_ref()
                .is_none_or(|(image, _)| !Arc::ptr_eq(image, &layer.image))
            {
                let image = &layer.image;
                let color = egui::ColorImage::from_rgba_unmultiplied(
                    [image.width() as usize, image.height() as usize],
                    image.as_raw(),
                );
                let texture =
                    ui.ctx()
                        .load_texture(layer.label(), color, egui::TextureOptions::LINEAR);
                *slot = Some((Arc::clone(image), texture));
            }
            let Some((_, texture)) = slot else { continue };
            let geometry = layer.geometry(rect.width(), rect.height());
            let corners = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].map(|(x, y)| {
                let [x, y] = geometry.point(x, y);
                rect.min + egui::vec2(x, y)
            });
            let mut mesh = egui::Mesh::with_texture(texture.id());
            for (pos, uv) in corners.into_iter().zip(uv) {
                mesh.vertices.push(egui::epaint::Vertex {
                    pos,
                    uv,
                    color: Color32::WHITE,
                });
            }
            mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
            painter.add(egui::Shape::mesh(mesh));
            if self.selected == Some(i) && editing == Some(layer.is_sticker()) {
                painter.add(egui::Shape::closed_line(
                    corners.to_vec(),
                    Stroke::new(1.0, theme::ACCENT),
                ));
            }
        }
    }
}

fn short_label(label: &str) -> String {
    label
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(26)
        .collect()
}
