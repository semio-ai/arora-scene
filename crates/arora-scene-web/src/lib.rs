//! A browser host for the scene, instrumented for measurement.
//!
//! The page fetches the robot itself and hands its bytes to [`start`], because
//! the scene's mount functions read the GLB from a filesystem a browser does
//! not have. The asset server still loads the same GLB over HTTP for the
//! scene; the bytes here are only for the declaration `RobotData` carries.
//!
//! Two things are recorded for the page to read back, both with the page's
//! own `performance.now()` clock so they compare directly with a three.js
//! page timed the same way:
//!
//! - **per frame**: when the frame began and how long the whole update and
//!   render took on the main thread, which is the CPU work regardless of what
//!   the display's refresh would allow;
//! - **load marks**: when `start` ran, the first frame, every robot copy
//!   spawned, the first frame in which every mesh the main camera sees has a
//!   compiled pipeline and is queued (the robot submitted whole), and when the
//!   GPU reported that frame's work done.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

use arora_scene::instance::{RobotMeta, Selected};
use arora_scene::{OrbitCamera, RobotSpec, SceneOptions, ScenePlugin};
use arora_visuals::active_area::ActiveArea;
use arora_visuals::select::Selection;
use arora_visuals::transform_gizmo::TransformModes;
use bevy::asset::AssetMetaCheck;
use bevy::pbr::{PendingMeshMaterialQueues, SpecializedMaterialPipelineCache};
use bevy::prelude::*;
use bevy::render::render_resource::{CachedPipelineState, PipelineCache};
use bevy::render::renderer::RenderQueue;
use bevy::render::view::{ExtractedView, RenderVisibleEntities, RetainedViewEntity};
use bevy::render::{Extract, ExtractSchedule, Render, RenderApp, RenderSystems};
use bevy::shader::ShaderCacheError;
use bevy::window::WindowResolution;
use bevy::winit::WinitSettings;
use bevy::world_serialization::WorldInstanceReady;
use serde::Deserialize;
use wasm_bindgen::prelude::*;

fn now() -> f64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map(|p| p.now())
        .unwrap_or(0.0)
}

/// What a run asks for, as JSON from the page.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Options {
    copies: u32,
    still: bool,
    shadows: Option<bool>,
    select: Vec<String>,
    gizmo: bool,
    /// The visible part of the canvas, `[x, y, width, height]` in CSS pixels,
    /// as Studio's `activeBounds`.
    panel: Option<[f32; 4]>,
    /// Render at this many physical pixels per CSS pixel instead of the
    /// page's `devicePixelRatio`.
    scale_factor: Option<f32>,
}

static FRAME_START: AtomicU64 = AtomicU64::new(0);
static FRAMES: Mutex<Vec<f64>> = Mutex::new(Vec::new());
static MARKS: Mutex<Vec<(String, f64)>> = Mutex::new(Vec::new());
static ROOTS_READY: AtomicU32 = AtomicU32::new(0);
static COPIES: AtomicU32 = AtomicU32::new(1);

fn mark(name: &str) {
    let t = now();
    MARKS.lock().unwrap().push((name.to_string(), t));
}

/// Frame records as a flat array of `[start, cpu]` pairs, emptied by reading.
#[wasm_bindgen]
pub fn take_frames() -> Vec<f64> {
    std::mem::take(&mut *FRAMES.lock().unwrap())
}

/// Load marks as JSON: `[[name, ms], ...]`.
#[wasm_bindgen]
pub fn marks() -> String {
    serde_json::to_string(&*MARKS.lock().unwrap()).unwrap_or_default()
}

#[wasm_bindgen]
pub fn start(canvas: String, glb: String, bytes: Vec<u8>, options: String) -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    mark("start");
    let opts: Options =
        serde_json::from_str(&options).map_err(|e| JsValue::from_str(&e.to_string()))?;
    let meta = arora_visuals::meta::AssetMeta::from_glb_bytes(&bytes)
        .map_err(|e| JsValue::from_str(&format!("{e:#}")))?;
    mark("meta-parsed");
    let copies = opts.copies.max(1);
    COPIES.store(copies, Ordering::Relaxed);

    let mut resolution = WindowResolution::default();
    if let Some(sf) = opts.scale_factor {
        resolution.set_scale_factor_override(Some(sf));
    }

    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    canvas: Some(canvas),
                    // Sized by its parent, which the page fixes in CSS pixels,
                    // as the three.js page's canvas is.
                    fit_canvas_to_parent: true,
                    prevent_default_event_handling: true,
                    resolution,
                    ..default()
                }),
                ..default()
            })
            .set(AssetPlugin {
                file_path: "assets".into(),
                // No `.meta` files are served; asking for them is a wasted
                // round trip per asset.
                meta_check: AssetMetaCheck::Never,
                ..default()
            }),
    )
    .insert_resource(WinitSettings::continuous())
    .add_plugins(ScenePlugin)
    .insert_resource(SceneOptions {
        still: opts.still,
        shadows: opts.shadows.unwrap_or(true),
        gizmo: opts.gizmo.then_some(TransformModes::ALL),
        ..default()
    })
    .insert_resource(RobotSpec { glb, copies })
    // What `arora_scene::mount_instances` does from a file, done from bytes.
    .insert_resource(RobotMeta(meta))
    .insert_resource(Selected(opts.select.iter().map(Selection::of).collect()))
    .add_systems(Update, arora_scene::instance::index_instances)
    .add_systems(First, frame_begins)
    .add_observer(|_: On<WorldInstanceReady>| {
        let n = ROOTS_READY.fetch_add(1, Ordering::Relaxed) + 1;
        if n == COPIES.load(Ordering::Relaxed) {
            mark("robots-spawned");
        }
    });
    if let Some([x, y, w, h]) = opts.panel {
        app.insert_resource(Panel(Rect::new(x, y, x + w, y + h)))
            .add_systems(Update, place_panel);
    }
    if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
        render_app
            .init_resource::<MainView>()
            .add_systems(ExtractSchedule, extract_main_view)
            .add_systems(
                Render,
                (watch_robot_drawn, frame_ends)
                    .chain()
                    .in_set(RenderSystems::Cleanup),
            );
    }
    mark("app-built");
    app.run();
    Ok(())
}

#[derive(Resource)]
struct Panel(Rect);

fn place_panel(
    panel: Res<Panel>,
    cameras: Query<Entity, (With<OrbitCamera>, Without<ActiveArea>)>,
    mut commands: Commands,
) {
    for camera in &cameras {
        commands.entity(camera).insert(ActiveArea(panel.0));
    }
}

fn frame_begins(mut first: Local<bool>) {
    if !*first {
        *first = true;
        mark("first-frame");
    }
    FRAME_START.store(now().to_bits(), Ordering::Relaxed);
}

fn frame_ends() {
    let start = f64::from_bits(FRAME_START.load(Ordering::Relaxed));
    let end = now();
    let mut frames = FRAMES.lock().unwrap();
    frames.push(start);
    frames.push(end - start);
}

#[derive(Resource, Default)]
struct MainView {
    view: Option<RetainedViewEntity>,
    done: bool,
}

fn extract_main_view(
    cameras: Extract<Query<Entity, With<OrbitCamera>>>,
    mut main: ResMut<MainView>,
) {
    main.view = cameras
        .iter()
        .next()
        .map(|camera| RetainedViewEntity::new(camera.into(), None, 0));
}

/// The first frame in which every robot has spawned and every mesh the main
/// camera sees is queued with a compiled pipeline: the robot submitted whole.
/// The GPU then says when that frame's work is done.
fn watch_robot_drawn(
    views: Query<(&ExtractedView, &RenderVisibleEntities)>,
    mut main: ResMut<MainView>,
    specialized: Res<SpecializedMaterialPipelineCache>,
    pending: Res<PendingMeshMaterialQueues>,
    pipelines: Res<PipelineCache>,
    queue: Res<RenderQueue>,
) {
    if main.done || ROOTS_READY.load(Ordering::Relaxed) < COPIES.load(Ordering::Relaxed) {
        return;
    }
    let Some(id) = main.view else { return };
    for (view, visible) in &views {
        if view.retained_view_entity != id {
            continue;
        }
        let view_pipelines = specialized.get(&id);
        let pending = pending.get(&id).map(|queues| &queues.current_frame);
        let mut seen = 0usize;
        let mut waiting = 0usize;
        for (entity, main_entity) in visible
            .get::<Mesh3d>()
            .into_iter()
            .flat_map(|meshes| meshes.iter_visible())
        {
            seen += 1;
            let queued = !pending.is_some_and(|p| p.contains(&(*entity, *main_entity)));
            let ready = view_pipelines
                .and_then(|ids| ids.get(main_entity))
                .is_some_and(|pipeline| settled(pipelines.get_render_pipeline_state(*pipeline)));
            if !(queued && ready) {
                waiting += 1;
            }
        }
        if seen == 0 || waiting > 0 {
            return;
        }
        main.done = true;
        mark("robot-submitted");
        queue.on_submitted_work_done(|| mark("robot-gpu-done"));
    }
}

fn settled(state: &CachedPipelineState) -> bool {
    match state {
        CachedPipelineState::Ok(_) => true,
        CachedPipelineState::Err(
            ShaderCacheError::ShaderNotLoaded(_) | ShaderCacheError::ShaderImportNotYetAvailable,
        ) => false,
        CachedPipelineState::Err(_) => true,
        CachedPipelineState::Queued | CachedPipelineState::Creating(_) => false,
    }
}
