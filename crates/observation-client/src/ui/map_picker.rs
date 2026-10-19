//! 2D slippy-map dialog for choosing the area to load.

use super::widgets::{hint, primary, secondary};
use crate::net::Tiles;
use crate::theme::{IMAGE_TINT, Tokens};
use bevy::asset::Handle;
use bevy::image::Image;
use bevy::prelude::Resource;
use bevy_egui::egui::{
    self, Align2, CornerRadius, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2, vec2,
};
use observation_core::geo::{BBox, LatLon, MAX_BBOX_SIDE_M, MIN_BBOX_SIDE_M};
use observation_core::tiles::{TileId, lat_to_y, lon_to_x, x_to_lon, y_to_lat};

const TILE_PX: f64 = 256.0;
const MIN_ZOOM: f64 = 3.0;
const MAX_ZOOM: f64 = 19.0;
/// How many zoom levels up a loaded tile may be stretched to fill a gap.
const MAX_OVERZOOM: u8 = 8;
/// Default square drawn by "Square here", metres.
const DEFAULT_SIDE_M: f64 = 300.0;
/// Fallback centre (the demo street).
const FALLBACK: LatLon = LatLon::new(47.70865, -122.206);

#[derive(Debug, Resource)]
pub struct MapPicker {
    center: LatLon,
    zoom: f64,
    selection: Option<BBox>,
    draw_mode: bool,
    draw_start: Option<LatLon>,
    canvas: Option<Rect>,
    visible: bool,
}

impl Default for MapPicker {
    fn default() -> Self {
        Self {
            center: FALLBACK,
            zoom: 17.0,
            selection: None,
            draw_mode: false,
            draw_start: None,
            canvas: None,
            visible: false,
        }
    }
}

/// Screen ⇄ map transform for the current view.
#[derive(Debug, Clone, Copy)]
struct View {
    z: u8,
    tile_px: f64,
    /// Fractional tile coordinates of the canvas centre.
    cx: f64,
    cy: f64,
    rect: Rect,
}

impl View {
    fn new(p: &MapPicker, rect: Rect) -> Self {
        let z = p.zoom.floor().clamp(0.0, MAX_ZOOM) as u8;
        let tile_px = TILE_PX * 2f64.powf(p.zoom - f64::from(z));
        Self {
            z,
            tile_px,
            cx: lon_to_x(p.center.lon, z),
            cy: lat_to_y(p.center.lat, z),
            rect,
        }
    }

    fn screen_of(&self, ll: LatLon) -> Pos2 {
        let c = self.rect.center();
        let x = (lon_to_x(ll.lon, self.z) - self.cx) * self.tile_px;
        let y = (lat_to_y(ll.lat, self.z) - self.cy) * self.tile_px;
        Pos2::new(c.x + x as f32, c.y + y as f32)
    }

    fn latlon_of(&self, p: Pos2) -> LatLon {
        let c = self.rect.center();
        let tx = self.cx + f64::from(p.x - c.x) / self.tile_px;
        let ty = self.cy + f64::from(p.y - c.y) / self.tile_px;
        LatLon::new(
            y_to_lat(ty, self.z).clamp(-85.0, 85.0),
            x_to_lon(tx, self.z).clamp(-180.0, 180.0),
        )
    }

    fn visible_tiles(&self) -> Vec<TileId> {
        let half_w = f64::from(self.rect.width()) / 2.0 / self.tile_px;
        let half_h = f64::from(self.rect.height()) / 2.0 / self.tile_px;
        let n = 1i64 << self.z;
        let (x0, x1) = (
            (self.cx - half_w).floor() as i64,
            (self.cx + half_w).floor() as i64,
        );
        let (y0, y1) = (
            (self.cy - half_h).floor() as i64,
            (self.cy + half_h).floor() as i64,
        );
        let mut out = Vec::new();
        for y in y0.max(0)..=y1.min(n - 1) {
            for x in x0.max(0)..=x1.min(n - 1) {
                if let Some(t) = TileId::new(self.z, x as u32, y as u32) {
                    out.push(t);
                }
            }
        }
        out
    }
}

impl MapPicker {
    /// Opens centred on `origin` (or the fallback), showing `bbox` if given.
    pub fn open_at(&mut self, origin: LatLon, bbox: Option<BBox>) {
        if origin.lat != 0.0 || origin.lon != 0.0 {
            self.center = origin;
        }
        self.selection = bbox;
        self.zoom = 17.0;
        self.draw_start = None;
    }

    fn zoom_about(&mut self, view: &View, anchor: Pos2, delta: f64) {
        let before = view.latlon_of(anchor);
        self.zoom = (self.zoom + delta).clamp(MIN_ZOOM, MAX_ZOOM);
        let v2 = View::new(self, view.rect);
        let after = v2.latlon_of(anchor);
        self.center = LatLon::new(
            (self.center.lat + before.lat - after.lat).clamp(-85.0, 85.0),
            self.center.lon + before.lon - after.lon,
        );
    }
}

/// Validation message for a selection, or `None` if it can be loaded.
fn problem(b: &BBox) -> Option<String> {
    let (w, h) = (b.width_m(), b.height_m());
    if w > MAX_BBOX_SIDE_M || h > MAX_BBOX_SIDE_M {
        Some(format!("Too large: sides must be ≤ {MAX_BBOX_SIDE_M:.0} m"))
    } else if w < MIN_BBOX_SIDE_M || h < MIN_BBOX_SIDE_M {
        Some(format!("Too small: sides must be ≥ {MIN_BBOX_SIDE_M:.0} m"))
    } else {
        None
    }
}

/// Part of an image drawn into part of a tile's square, both as `0..1` fractions.
#[derive(Debug, Clone, Copy)]
pub struct TileDraw {
    tile: TileId,
    texture: egui::TextureId,
    /// Where in the tile's square to draw.
    dest: Rect,
    /// Which part of the texture to draw there.
    uv: Rect,
}

/// `[left, top, right, bottom]` fractions as a rect.
fn frac_rect([l, t, r, b]: [f32; 4]) -> Rect {
    Rect::from_min_max(Pos2::new(l, t), Pos2::new(r, b))
}

/// Requests the tiles the dialog will draw this frame and returns what to draw
/// for each, in drawing order. A tile that is still loading is filled from a
/// loaded ancestor (stretched) and any loaded children, so zooming shows a
/// blurry map rather than an empty canvas.
pub fn register_tiles(
    p: &mut MapPicker,
    tiles: &mut Tiles,
    mut add_image: impl FnMut(&Handle<Image>) -> egui::TextureId,
) -> Vec<TileDraw> {
    let mut out = Vec::new();
    if !p.visible {
        return out;
    }
    let Some(rect) = p.canvas else { return out };
    let view = View::new(p, rect);
    let wanted = view.visible_tiles();
    tiles.retain_queue(|t| t.z != view.z || wanted.contains(t));
    let unit = frac_rect([0.0, 0.0, 1.0, 1.0]);
    for tile in wanted {
        if let Some(h) = tiles.get(tile) {
            out.push(TileDraw {
                tile,
                texture: add_image(&h),
                dest: unit,
                uv: unit,
            });
            continue;
        }
        let ancestor = (1..=MAX_OVERZOOM)
            .filter_map(|up| tile.within_ancestor(up))
            .find_map(|(a, f)| Some((tiles.ready(a)?, f)));
        if let Some((h, f)) = ancestor {
            out.push(TileDraw {
                tile,
                texture: add_image(&h),
                dest: unit,
                uv: frac_rect(f),
            });
        }
        for c in tile.children().into_iter().flatten() {
            if let (Some(h), Some((_, f))) = (tiles.ready(c), c.within_ancestor(1)) {
                out.push(TileDraw {
                    tile,
                    texture: add_image(&h),
                    dest: frac_rect(f),
                    uv: unit,
                });
            }
        }
    }
    out
}

/// Shows the dialog. Returns the chosen box when the user confirms.
pub fn window(
    ctx: &egui::Context,
    t: &Tokens,
    p: &mut MapPicker,
    tile_draws: &[TileDraw],
    attribution: &str,
    open: &mut bool,
) -> Option<BBox> {
    let mut result = None;
    let screen = ctx.viewport_rect();
    let size = vec2(
        (screen.width() * 0.8).min(1100.0),
        (screen.height() * 0.8).min(800.0),
    );

    // Scrim behind the dialog.
    egui::Area::new(egui::Id::new("map-scrim"))
        .order(egui::Order::Middle)
        .fixed_pos(Pos2::ZERO)
        .show(ctx, |ui| {
            ui.painter().rect_filled(screen, 0.0, t.scrim);
            if ui.allocate_rect(screen, Sense::click()).clicked() {
                *open = false;
            }
        });

    egui::Window::new("Choose an area")
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .title_bar(false)
        .fixed_size(size)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .frame(t.card())
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Choose an area").size(t.font.title).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(secondary(t, "Close")).clicked() {
                        *open = false;
                    }
                });
            });
            hint(
                ui,
                t,
                &format!(
                    "Drag to pan, scroll to zoom. Turn on Draw area (or hold Shift) and drag to draw a box up to {MAX_BBOX_SIDE_M:.0} m × {MAX_BBOX_SIDE_M:.0} m."
                ),
            );
            ui.add_space(t.space.xs);

            let footer_h = t.layout.hit_target + t.space.lg;
            let map_size = vec2(ui.available_width(), (ui.available_height() - footer_h).max(200.0));
            let (rect, resp) = ui.allocate_exact_size(map_size, Sense::click_and_drag());
            p.canvas = Some(rect);
            p.visible = true;
            let view = View::new(p, rect);
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, CornerRadius::same(t.radius.md), t.surface_sunken);

            // Tiles.
            let ts = view.tile_px as f32;
            for tile in view.visible_tiles() {
                let nw = view.screen_of(tile.nw());
                let tr = Rect::from_min_size(nw, vec2(ts, ts));
                let mut drawn = false;
                for d in tile_draws.iter().filter(|d| d.tile == tile) {
                    let dest = Rect::from_min_max(tr.lerp_inside(d.dest.min.to_vec2()), tr.lerp_inside(d.dest.max.to_vec2()));
                    painter.image(d.texture, dest, d.uv, IMAGE_TINT);
                    drawn = true;
                }
                if !drawn {
                    painter.rect_stroke(tr, 0.0, Stroke::new(t.stroke_width, t.border), StrokeKind::Inside);
                }
            }

            // Interaction.
            let shift = ui.input(|i| i.modifiers.shift);
            let drawing = p.draw_mode || shift;
            if resp.drag_started()
                && drawing
                && let Some(pos) = resp.interact_pointer_pos()
            {
                p.draw_start = Some(view.latlon_of(pos));
            }
            if resp.dragged() {
                match (p.draw_start, resp.interact_pointer_pos()) {
                    (Some(a), Some(pos)) => {
                        let b = view.latlon_of(pos);
                        p.selection = BBox::new(a.lat.min(b.lat), a.lon.min(b.lon), a.lat.max(b.lat), a.lon.max(b.lon)).ok();
                    }
                    _ => {
                        let d = resp.drag_delta();
                        let centre = rect.center() - d;
                        p.center = view.latlon_of(centre);
                    }
                }
            }
            if resp.drag_stopped() {
                p.draw_start = None;
            }
            if resp.hovered() {
                let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                if scroll != 0.0
                    && let Some(pos) = resp.hover_pos()
                {
                    p.zoom_about(&view, pos, f64::from(scroll) / 240.0);
                }
            }
            let view = View::new(p, rect);

            // Selection box.
            if let Some(b) = p.selection {
                let r = Rect::from_two_pos(
                    view.screen_of(LatLon::new(b.north, b.west)),
                    view.screen_of(LatLon::new(b.south, b.east)),
                );
                let bad = problem(&b).is_some();
                let col = if bad { t.danger } else { t.selection_box };
                painter.rect_filled(r, 0.0, col.gamma_multiply(0.15));
                painter.rect_stroke(r, 0.0, Stroke::new(t.stroke_width * 2.0, col), StrokeKind::Middle);
                painter.text(
                    r.left_top() + vec2(t.space.xs, -t.space.xs),
                    Align2::LEFT_BOTTOM,
                    format!("{:.0} × {:.0} m", b.width_m(), b.height_m()),
                    egui::FontId::proportional(t.font.caption),
                    col,
                );
            }
            // Attribution (required by the tile policy).
            let galley = painter.layout_no_wrap(attribution.to_owned(), egui::FontId::proportional(t.font.caption), t.text);
            let ar = Rect::from_min_size(rect.right_bottom() - galley.size() - vec2(t.space.sm, t.space.xs) * 2.0, galley.size() + vec2(t.space.sm, t.space.xs) * 2.0);
            painter.rect_filled(ar, CornerRadius::same(t.radius.sm), t.surface_raised.gamma_multiply(0.85));
            painter.galley(ar.min + vec2(t.space.sm, t.space.xs), galley, t.text);

            // Footer.
            ui.add_space(t.space.sm);
            ui.horizontal(|ui| {
                if ui.add(secondary(t, "−")).on_hover_text("Zoom out").clicked() {
                    p.zoom_about(&view, rect.center(), -1.0);
                }
                if ui.add(secondary(t, "+")).on_hover_text("Zoom in").clicked() {
                    p.zoom_about(&view, rect.center(), 1.0);
                }
                let draw_label = if p.draw_mode { "Drawing: on" } else { "Draw area" };
                if ui.add(secondary(t, draw_label)).on_hover_text("Drag on the map to draw a box (Shift)").clicked() {
                    p.draw_mode = !p.draw_mode;
                }
                if ui.add(secondary(t, "Square here")).on_hover_text("A 300 m box at the map centre").clicked() {
                    p.selection = Some(BBox::around(p.center, DEFAULT_SIDE_M));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let valid = p.selection.filter(|b| problem(b).is_none());
                    if ui.add_enabled(valid.is_some(), primary(t, "Load area")).clicked() {
                        result = valid;
                        *open = false;
                    }
                    match p.selection.as_ref().and_then(problem) {
                        Some(msg) => {
                            ui.label(RichText::new(msg).color(t.danger));
                        }
                        None if p.selection.is_none() => hint(ui, t, "No area drawn yet"),
                        None => {}
                    }
                });
            });
        });
    if !*open {
        p.visible = false;
    }
    result
}
