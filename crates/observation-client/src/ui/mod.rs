//! egui front-end: docked panels around the 3D viewport, plus overlays.

mod actions;
mod agent;
mod inspector;
pub mod map_picker;
mod overlays;
mod provenance;
mod regions;
mod scenes;
mod sheet;
mod toolbar;
mod widgets;

use crate::coverage::CoverageState;
use crate::history::{HistoryCommand, HistoryOp};
use crate::net::Tiles;
use crate::persist::PersistAction;
use crate::platform::Platform;
use crate::state::{
    Area, DataChangeNotice, Editor, Hover, LoadArea, Loading, Models, Panels, Scene, ToastAction,
    Toasts, Tool,
};
use crate::theme::{
    MIN_VIEWPORT, SchemeChanges, Tokens, UiFont, follow_scheme, install_fonts, strong_family,
};
use bevy::camera::Viewport;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::{EguiContexts, EguiPrimaryContextPass, EguiTextureHandle, egui};
use map_picker::MapPicker;
use observation_client_model::vm::LoadingVm;

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        let changes = SchemeChanges::watch(&*Platform::of(app.world()).scheme);
        app.init_resource::<MapPicker>()
            .init_resource::<sheet::SheetResults>()
            .insert_resource(changes)
            // Before every `Tokens` reader, so the panels and the scene switch
            // in the same frame.
            .add_systems(PreUpdate, follow_scheme)
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

/// Wall-clock time: the virtual clock caps each frame at 250 ms, which would
/// slow toasts and the loading timer whenever frames are sparse.
fn tick_toasts(time: Res<Time<Real>>, mut toasts: ResMut<Toasts>) {
    let dt = time.delta_secs();
    if toasts.is_empty() {
        return;
    }
    toasts.tick(dt);
}

/// Everything the panels need, bundled to keep system signatures readable.
#[derive(bevy::ecs::system::SystemParam)]
pub struct UiData<'w> {
    pub tokens: Res<'w, Tokens>,
    /// Wall-clock time, for "saved just now" and the loading timer.
    pub time: Res<'w, Time<Real>>,
    pub data_change: ResMut<'w, DataChangeNotice>,
    pub save: Res<'w, crate::persist::Autosave>,
    /// The scenes saved in this browser, and the Scenes dialog's rename and
    /// delete in progress.
    pub slots: ResMut<'w, crate::persist::SceneSlots>,
    pub editor: ResMut<'w, Editor>,
    pub panels: ResMut<'w, Panels>,
    pub hover: ResMut<'w, Hover>,
    pub toasts: ResMut<'w, Toasts>,
    pub scene: ResMut<'w, Scene>,
    pub models: ResMut<'w, Models>,
    pub coverage: Res<'w, CoverageState>,
    /// The splits and verdicts worked out for the panels.
    pub footage: ResMut<'w, crate::state::Footage>,
    pub addresses: Res<'w, crate::addresses::CameraAddresses>,
    pub area: Option<Res<'w, Area>>,
    pub loading: ResMut<'w, Loading>,
    pub history: Res<'w, crate::history::History>,
    pub history_commands: MessageWriter<'w, HistoryCommand>,
    pub tiles: ResMut<'w, Tiles>,
    pub picker: ResMut<'w, MapPicker>,
    pub place_search: ResMut<'w, crate::net::PlaceSearch>,
    pub config: Res<'w, crate::net::ServerConfig>,
    pub layout: ResMut<'w, crate::state::UiLayout>,
    pub font: ResMut<'w, UiFont>,
    pub address_jump: ResMut<'w, crate::state::AddressJump>,
    pub regions: ResMut<'w, crate::regions::Regions>,
    /// The link to a local agent (Connect agent), and the proposed scene's
    /// figures for the proposal card.
    pub agent: ResMut<'w, crate::agent::AgentLinkRes>,
    pub proposal_coverage: Res<'w, crate::coverage::ProposalCoverage>,
}

fn draw_ui(
    mut contexts: EguiContexts,
    mut d: UiData,
    mut camera: Query<(&mut Camera, &GlobalTransform), With<crate::input::OrbitCamera>>,
    window: Single<&Window, With<PrimaryWindow>>,
    mut load: MessageWriter<LoadArea>,
    mut persist: MessageWriter<PersistAction>,
    platform: Res<Platform>,
    sheet_results: Res<sheet::SheetResults>,
) -> Result {
    let clock = &*platform.clock;
    // Register tile textures with egui before borrowing the context.
    let tile_draws = map_picker::register_tiles(&mut d.picker, &mut d.tiles, clock, |h| {
        contexts.add_image(EguiTextureHandle::Weak(h.id()))
    });
    let ctx = contexts.ctx_mut()?.clone();
    let t = *d.tokens;
    if d.area.as_ref().is_some_and(|a| a.is_changed()) {
        // Cells and building indices of the old area mean nothing now.
        d.footage.reset();
    }
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
    if let Ok((mut cam, _)) = camera.single_mut() {
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

    overlays::mode_pill(&ctx, &t, viewport_rect, &d);
    // Quick actions next to the selected camera, where the eye already is.
    let anchor = d
        .editor
        .selected
        .filter(|_| {
            !matches!(
                d.editor.tool,
                Tool::Place | Tool::Areas | Tool::Target | Tool::Address
            )
        })
        .and_then(|id| d.scene.camera(id))
        .zip(camera.single().ok())
        .and_then(|(c, (cam, gt))| cam.world_to_viewport(gt, Vec3::from_array(c.position)).ok())
        .map(|p| egui::pos2(p.x, p.y))
        .filter(|p| viewport_rect.shrink(t.space.lg).contains(*p));
    if let (Some(id), Some(at)) = (d.editor.selected, anchor)
        && let Some(a) = overlays::camera_toolbar(&ctx, &t, viewport_rect, at)
    {
        actions::apply(&mut d, id, a);
    }
    overlays::tool_cursor(&ctx, viewport_rect, d.editor.tool);
    overlays::address_card(&ctx, &t, viewport_rect, &d.hover, &d.editor);
    match overlays::toasts(&ctx, &t, viewport_rect, &d.toasts) {
        Some(overlays::ToastEvent::Action(ToastAction::Undo)) => {
            d.history_commands.write(HistoryCommand(HistoryOp::Undo));
            d.toasts.forget_undo();
        }
        Some(overlays::ToastEvent::Action(ToastAction::Retry(req))) => {
            d.toasts.forget_retry();
            load.write(LoadArea::from(req));
        }
        Some(overlays::ToastEvent::Dismiss(i)) if i < d.toasts.len() => {
            d.toasts.dismiss(i);
        }
        _ => {}
    }
    if let Some(vm) = LoadingVm::new(&d.loading, d.time.elapsed_secs_f64()) {
        match overlays::loading(&ctx, &t, viewport_rect, &vm) {
            Some(overlays::LoadingChoice::Cancel) => {
                d.loading.cancel();
            }
            Some(overlays::LoadingChoice::Demo) => {
                load.write(LoadArea::demo());
            }
            None => {}
        }
    }
    if d.panels.help_open {
        overlays::help(&ctx, &t, &mut d.panels);
    }
    sheet::drain(&sheet_results, &mut d);
    if d.panels.sheet_open {
        sheet::window(&ctx, &t, &mut d, &platform, &sheet_results);
    }
    if d.agent.dialog_open {
        agent::window(&ctx, &t, &mut d.agent, &*platform.http);
    }
    if d.agent.card_open {
        agent::proposal_card(&ctx, &t, &mut d, viewport_rect, &*platform.http, clock);
    }
    if d.panels.regions_open {
        regions::window(&ctx, &t, &mut d.regions, &mut d.panels.regions_open, clock);
    }
    if d.panels.scenes_open {
        let busy = d.loading.area().is_some();
        let has_area = d.scene.area.is_some();
        scenes::window(
            &ctx,
            &t,
            &mut d.slots,
            busy,
            has_area,
            &mut d.panels.scenes_open,
            &mut persist,
        );
    }
    let local: Vec<(String, observation_core::geo::BBox)> = d
        .regions
        .regions()
        .iter()
        .filter(|r| r.ready)
        .filter_map(|r| Some((r.name.clone(), r.bbox?)))
        .collect();
    if d.panels.map_open
        && let Some(bbox) = map_picker::window(
            &ctx,
            &t,
            &mut d.picker,
            &mut d.place_search,
            &tile_draws,
            &local,
            &d.config,
            &mut d.panels.map_open,
        )
    {
        load.write(LoadArea::osm(bbox));
    }
    Ok(())
}
