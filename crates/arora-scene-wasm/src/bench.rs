//! Load and frame-time measurement for the render benchmark, and the
//! diagnostic switches it compares runs with.
//!
//! Everything is recorded with the page's own `performance.now()` clock, so
//! it compares directly with a three.js page timed the same way:
//!
//! - **per frame**: when the frame began and how long the whole update and
//!   render took on the main thread, which is the CPU work regardless of what
//!   the display's refresh would allow;
//! - **load marks**: when `mount` ran, the first frame, every robot copy
//!   spawned, the first frame in which every mesh the main camera sees has a
//!   compiled pipeline and is queued (the robot submitted whole), and when the
//!   GPU reported that frame's work done;
//! - **push latency**: from a `set_value` call to the end of the frame that
//!   drew it.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

use arora_scene::OrbitCamera;
use bevy::pbr::{PendingMeshMaterialQueues, SpecializedMaterialPipelineCache};
use bevy::prelude::*;
use bevy::render::render_resource::{CachedPipelineState, PipelineCache};
use bevy::render::renderer::RenderQueue;
use bevy::render::view::{ExtractedView, RenderVisibleEntities, RetainedViewEntity};
use bevy::render::{Extract, ExtractSchedule, Render, RenderApp, RenderSystems};
use bevy::shader::ShaderCacheError;
use bevy::winit::WinitSettings;
use bevy::world_serialization::WorldInstanceReady;
use serde::Deserialize;
use wasm_bindgen::prelude::*;

use crate::now;

/// What a benchmark run asks for beyond a normal mount.
#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Options {
    /// How many copies of the robot to draw.
    pub copies: u32,
    /// Hold the camera still instead of letting it settle on the framing.
    pub still: bool,
    /// Draw every frame, as a game loop does, instead of only frames in
    /// which something changed.
    pub continuous: bool,
    /// Draw every mesh into every view, the shadow cascades among them,
    /// without testing its bounds against the view first.
    pub no_culling: bool,
    /// Build each frame's per-mesh data on the CPU instead of in a compute
    /// pass, as a platform without compute shaders does.
    pub cpu_preprocess: bool,
    /// Hold every light's shadows to one cascade reaching this far from the
    /// camera, instead of the scene's reach that follows the framing.
    pub shadow_reach: Option<f32>,
}

static FRAME_START: AtomicU64 = AtomicU64::new(0);
static FRAMES: Mutex<Vec<f64>> = Mutex::new(Vec::new());
static MARKS: Mutex<Vec<(String, f64)>> = Mutex::new(Vec::new());
static ROOTS_READY: AtomicU32 = AtomicU32::new(0);
static COPIES: AtomicU32 = AtomicU32::new(1);
/// When each push the current frame applied was pushed.
static APPLYING: Mutex<Vec<f64>> = Mutex::new(Vec::new());
/// Push-to-drawn times, in milliseconds.
static LATENCIES: Mutex<Vec<f64>> = Mutex::new(Vec::new());
/// The orbit camera's position as last updated, then its near and far planes.
static CAMERA: Mutex<[f32; 5]> = Mutex::new([0.0; 5]);

pub fn mark(name: &str) {
    let t = now();
    MARKS.lock().unwrap().push((name.to_string(), t));
}

/// Notes that a push made at `at` is applied in the current frame.
pub fn applying(at: f64) {
    APPLYING.lock().unwrap().push(at);
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

/// Push-to-drawn times since the last read, in milliseconds.
#[wasm_bindgen]
pub fn take_latencies() -> Vec<f64> {
    std::mem::take(&mut *LATENCIES.lock().unwrap())
}

/// The orbit camera's position in world space, then its near and far planes.
#[wasm_bindgen]
pub fn camera_position() -> Vec<f32> {
    CAMERA.lock().unwrap().to_vec()
}

/// Adds the measurement systems and whichever diagnostic switches `opts`
/// turns on. Returns whether the run draws continuously, in which case the
/// caller must not install the draw-on-change plugin.
pub fn install(app: &mut App, opts: &Options) -> bool {
    COPIES.store(opts.copies.max(1), Ordering::Relaxed);
    app.add_systems(First, frame_begins)
        .add_systems(Last, record_camera)
        .add_observer(|_: On<WorldInstanceReady>| {
            let n = ROOTS_READY.fetch_add(1, Ordering::Relaxed) + 1;
            if n == COPIES.load(Ordering::Relaxed) {
                mark("robots-spawned");
            }
        });
    if opts.continuous {
        app.insert_resource(WinitSettings::continuous());
    }
    if opts.no_culling {
        app.add_systems(PostUpdate, never_cull);
    }
    if let Some(reach) = opts.shadow_reach {
        app.insert_resource(ShadowReach(reach)).add_systems(
            PostUpdate,
            hold_shadow_reach
                .before(bevy::light::SimulationLightSystems::UpdateDirectionalLightCascades),
        );
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
    opts.continuous
}

fn record_camera(cameras: Query<(&GlobalTransform, &Projection), With<OrbitCamera>>) {
    if let Ok((camera, projection)) = cameras.single() {
        let [x, y, z] = camera.translation().to_array();
        let (near, far) = match projection {
            Projection::Perspective(p) => (p.near, p.far),
            Projection::Orthographic(o) => (o.near, o.far),
            _ => (f32::NAN, f32::NAN),
        };
        *CAMERA.lock().unwrap() = [x, y, z, near, far];
    }
}

fn never_cull(
    meshes: Query<
        Entity,
        (
            With<Mesh3d>,
            Without<bevy::camera::visibility::NoFrustumCulling>,
        ),
    >,
    mut commands: Commands,
) {
    for mesh in &meshes {
        commands
            .entity(mesh)
            .insert(bevy::camera::visibility::NoFrustumCulling);
    }
}

#[derive(Resource)]
struct ShadowReach(f32);

fn hold_shadow_reach(
    reach: Res<ShadowReach>,
    mut lights: Query<&mut bevy::light::CascadeShadowConfig>,
) {
    let held = bevy::light::CascadeShadowConfigBuilder {
        num_cascades: 1,
        minimum_distance: 0.0,
        maximum_distance: reach.0,
        ..default()
    }
    .build();
    for mut config in &mut lights {
        if config.bounds != held.bounds {
            *config = held.clone();
        }
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
    // Every push this frame applied has now been drawn.
    let drawn = std::mem::take(&mut *APPLYING.lock().unwrap());
    LATENCIES
        .lock()
        .unwrap()
        .extend(drawn.into_iter().map(|pushed| end - pushed));
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
