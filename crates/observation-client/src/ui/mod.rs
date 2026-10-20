//! egui front-end: docked panels around the 3D viewport, plus overlays.

mod inspector;
pub mod map_picker;
mod overlays;
mod toolbar;
mod widgets;

use crate::coverage::CoverageState;
use crate::net::Tiles;
use crate::persist::PersistAction;
use crate::state::{Area, LoadArea, Loading, Models, Scene, UiState};
use crate::theme::{MIN_VIEWPORT, Tokens, UiFont, install_fonts, strong_family};
use bevy::camera::Viewport;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::{EguiContexts, EguiPrimaryContextPass, EguiTextureHandle, egui};
use map_picker::MapPicker;

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MapPicker>()
            .add_systems(EguiPrimaryContextPass, (apply_theme, draw_ui).chain())
            .add_systems(Update, tick_toasts);
    }
}

fn apply_theme(
    mut contexts: EguiContexts,
    tokens: Res<Tokens>,
    font: Res<UiFont>,
    mut done: Local<bool>,
) -> Result {
    let ctx = contexts.ctx_mut()?;
    if !*done || font.is_changed() {
        install_fonts(ctx, *font);
    }
    if !*done || tokens.is_changed() {
        tokens.apply(ctx);
    }
    *done = true;
    Ok(())
}

fn tick_toasts(time: Res<Time>, mut ui: ResMut<UiState>) {
    let dt = time.delta_secs();
    if ui.toasts.is_empty() {
        return;
    }
    for t in &mut ui.toasts {
        t.ttl -= dt;
    }
    ui.toasts.retain(|t| t.ttl > 0.0);
}

/// Everything the panels need, bundled to keep system signatures readable.
#[derive(bevy::ecs::system::SystemParam)]
pub struct UiData<'w> {
    pub tokens: Res<'w, Tokens>,
    pub ui: ResMut<'w, UiState>,
    pub scene: ResMut<'w, Scene>,
    pub models: ResMut<'w, Models>,
    pub coverage: Res<'w, CoverageState>,
    pub area: Option<Res<'w, Area>>,
    pub loading: Res<'w, Loading>,
    pub tiles: ResMut<'w, Tiles>,
    pub picker: ResMut<'w, MapPicker>,
    pub config: Res<'w, crate::net::ServerConfig>,
    pub layout: ResMut<'w, crate::state::UiLayout>,
    pub font: ResMut<'w, UiFont>,
}

fn draw_ui(
    mut contexts: EguiContexts,
    mut d: UiData,
    mut camera: Query<&mut Camera, With<crate::input::OrbitCamera>>,
    window: Single<&Window, With<PrimaryWindow>>,
    mut load: MessageWriter<LoadArea>,
    mut persist: MessageWriter<PersistAction>,
) -> Result {
    // Register tile textures with egui before borrowing the context.
    let tile_draws = map_picker::register_tiles(&mut d.picker, &mut d.tiles, |h| {
        contexts.add_image(EguiTextureHandle::Weak(h.id()))
    });
    let ctx = contexts.ctx_mut()?.clone();
    let t = *d.tokens;
    // Fonts set by `apply_theme` take effect from the next pass; until then
    // the UI's font families do not exist and laying out text would panic.
    if !ctx.fonts(|f| f.definitions().families.contains_key(&strong_family())) {
        return Ok(());
    }

    let small = window.width() < MIN_VIEWPORT.x || window.height() < MIN_VIEWPORT.y;
    if small {
        // The gate covers everything: no pointer position belongs to the scene.
        d.layout.viewport = Some(Rect::default());
        overlays::viewport_gate(&ctx, &t, window.width(), window.height());
        return Ok(());
    }

    let mut root = egui::Ui::new(
        ctx.clone(),
        "root".into(),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(ctx.viewport_rect()),
    );

    let top = egui::Panel::top("topbar")
        .exact_size(t.layout.topbar_height)
        .frame(
            t.panel()
                .inner_margin(egui::Margin::symmetric(t.space.md as i8, t.space.sm as i8)),
        )
        .show(&mut root, |ui| {
            toolbar::top_bar(ui, &mut d, &mut load, &mut persist)
        })
        .response
        .rect
        .height();
    let bottom = egui::Panel::bottom("statusbar")
        .exact_size(t.layout.statusbar_height)
        .frame(
            t.panel()
                .inner_margin(egui::Margin::symmetric(t.space.md as i8, t.space.xs as i8)),
        )
        .show(&mut root, |ui| toolbar::status_bar(ui, &d))
        .response
        .rect
        .height();
    let left = egui::Panel::left("tools")
        .exact_size(t.layout.toolbar_width)
        .resizable(false)
        .frame(t.panel().inner_margin(egui::Margin::same(t.space.sm as i8)))
        .show(&mut root, |ui| toolbar::tool_rail(ui, &mut d))
        .response
        .rect
        .width();
    // Keep at least ~60% of the width for the 3D view on portrait tablets.
    let max_inspector = (window.width() * 0.4).clamp(260.0, 440.0);
    let right = egui::Panel::right("inspector")
        .default_size(t.layout.inspector_width.min(max_inspector))
        .size_range(260.0..=max_inspector)
        .resizable(true)
        .frame(t.panel())
        .show(&mut root, |ui| inspector::inspector(ui, &mut d))
        .response
        .rect
        .width();

    // Fit the 3D viewport to the space between the panels.
    let sf = window.scale_factor();
    if let Ok(mut cam) = camera.single_mut() {
        let pos = UVec2::new((left * sf) as u32, (top * sf) as u32);
        let full = UVec2::new(window.physical_width(), window.physical_height());
        let size = full
            .saturating_sub(pos + UVec2::new((right * sf) as u32, (bottom * sf) as u32))
            .max(UVec2::ONE);
        let vp = Viewport {
            physical_position: pos,
            physical_size: size,
            ..default()
        };
        if cam
            .viewport
            .as_ref()
            .is_none_or(|v| v.physical_position != pos || v.physical_size != size)
        {
            cam.viewport = Some(vp);
        }
    }
    let viewport_rect = egui::Rect::from_min_max(
        egui::pos2(left, top),
        egui::pos2(window.width() - right, window.height() - bottom),
    );
    d.layout.viewport = Some(Rect::new(
        viewport_rect.min.x,
        viewport_rect.min.y,
        viewport_rect.max.x,
        viewport_rect.max.y,
    ));

    overlays::mode_banner(&ctx, &t, viewport_rect, &d);
    overlays::toasts(&ctx, &t, viewport_rect, &d.ui);
    if d.loading.area.is_some() {
        overlays::loading(&ctx, &t, viewport_rect);
    }
    if d.ui.help_open {
        overlays::help(&ctx, &t, &mut d.ui);
    }
    if d.ui.map_open
        && let Some(bbox) = map_picker::window(
            &ctx,
            &t,
            &mut d.picker,
            &tile_draws,
            &d.config.tile_attribution,
            &mut d.ui.map_open,
        )
    {
        load.write(LoadArea::osm(bbox));
    }
    Ok(())
}
