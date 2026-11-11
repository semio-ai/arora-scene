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
use arora_scene::redraw::RedrawOnChangePlugin;
use arora_scene::stage::Environment;
use arora_scene::values::{ApplyValues, ValueWrite, ValueWrites};
use arora_visuals::meta::FeatureKind;
use arora_visuals::value::Decoded;
use bevy::winit::{EventLoopProxyWrapper, WinitUserEvent};
use std::cell::RefCell;
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
    /// `"sky"` for the sky environment, which under the dark theme shows the
    /// aurora; anything else is the plain ground colour.
    environment: Option<String>,
    /// The camera's starting height above the model, in radians; negative
    /// looks up from below its middle.
    pitch: Option<f32>,
    /// The camera's starting yaw about the model, in radians.
    angle: Option<f32>,
    /// Draw every frame, as a game loop does, instead of only frames in
    /// which something changed.
    continuous: bool,
    /// Draw every mesh into every view, the shadow cascades among them,
    /// without testing its bounds against the view first.
    no_culling: bool,
    /// Build each frame's per-mesh data on the CPU instead of in a compute
    /// pass, as a platform without compute shaders does.
    cpu_preprocess: bool,
    /// Hold every light's shadows to one cascade reaching this far from the
    /// camera, instead of the scene's reach that follows the framing.
    shadow_reach: Option<f32>,
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
            .build()
            // Present because a dependency compiles code against it, not
            // because the scene uses it.
            .disable::<bevy::state::app::StatesPlugin>()
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
            })
            .set(bevy::pbr::PbrPlugin {
                use_gpu_instance_buffer_builder: !opts.cpu_preprocess,
                ..default()
            }),
    )
    .add_plugins(ScenePlugin)
    .insert_resource(SceneOptions {
        still: opts.still,
        shadows: opts.shadows.unwrap_or(true),
        gizmo: opts.gizmo.then_some(TransformModes::ALL),
        environment: match opts.environment.as_deref() {
            Some("sky") => Environment::Sky,
            _ => Environment::Plain,
        },
        pitch: opts.pitch.unwrap_or(SceneOptions::default().pitch),
        angle: opts.angle.unwrap_or(SceneOptions::default().angle),
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
    if opts.continuous {
        app.insert_resource(WinitSettings::continuous());
    } else {
        app.add_plugins(RedrawOnChangePlugin);
    }
    app.add_systems(Startup, keep_waker)
        .add_systems(Update, take_pushed.before(ApplyValues));
    app.add_systems(Last, record_camera);
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

/// The orbit camera's position as last updated, then its near and far
/// planes, for a page to read back.
static CAMERA: Mutex<[f32; 5]> = Mutex::new([0.0; 5]);

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

/// The orbit camera's position in world space, then its near and far planes.
#[wasm_bindgen]
pub fn camera_position() -> Vec<f32> {
    CAMERA.lock().unwrap().to_vec()
}

fn never_cull(
    meshes: Query<Entity, (With<Mesh3d>, Without<bevy::camera::visibility::NoFrustumCulling>)>,
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
    // Every push this frame applied has now been drawn.
    let drawn = std::mem::take(&mut *APPLYING.lock().unwrap());
    LATENCIES
        .lock()
        .unwrap()
        .extend(drawn.into_iter().map(|pushed| end - pushed));
}

thread_local! {
    /// Wakes a reactive event loop from outside it. The page calls in between
    /// frames, when the loop may be waiting indefinitely.
    static WAKE: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
}

/// Pushes waiting for the next frame, each with when it was pushed.
static PUSHED: Mutex<Vec<(ValueWrite, f64)>> = Mutex::new(Vec::new());
/// When each push the current frame applied was pushed.
static APPLYING: Mutex<Vec<f64>> = Mutex::new(Vec::new());
/// Push-to-drawn times, in milliseconds: from the call to the end of the
/// frame that drew it.
static LATENCIES: Mutex<Vec<f64>> = Mutex::new(Vec::new());

fn keep_waker(proxy: Res<EventLoopProxyWrapper>) {
    let proxy = (**proxy).clone();
    WAKE.with(|wake| {
        *wake.borrow_mut() = Some(Box::new(move || {
            let _ = proxy.send_event(WinitUserEvent::WakeUp);
        }));
    });
}

/// Hands the page's pushes to the scene's one place for applied values.
fn take_pushed(mut writes: ResMut<ValueWrites>) {
    let pushed = std::mem::take(&mut *PUSHED.lock().unwrap());
    if pushed.is_empty() {
        return;
    }
    let mut applying = APPLYING.lock().unwrap();
    for (write, at) in pushed {
        writes.0.push(write);
        applying.push(at);
    }
}

/// Writes one feature value: `target` an animatable or element id, `feature`
/// as `RobotData` names it (`rotation`, `translation`, `color`, ...), and
/// one value or three.
#[wasm_bindgen]
pub fn push_value(target: String, feature: String, values: Vec<f32>) -> Result<(), JsValue> {
    let feature = FeatureKind::from_name(&feature, &[])
        .ok_or_else(|| JsValue::from_str(&format!("unknown feature {feature:?}")))?;
    let value = match values.as_slice() {
        [s] => Decoded::Scalar(*s),
        [x, y, z] => Decoded::Triple([*x, *y, *z]),
        _ => return Err(JsValue::from_str("one value or three")),
    };
    PUSHED.lock().unwrap().push((
        ValueWrite {
            instance: None,
            target,
            feature,
            value,
        },
        now(),
    ));
    WAKE.with(|wake| {
        if let Some(wake) = wake.borrow().as_ref() {
            wake();
        }
    });
    Ok(())
}

/// Push-to-drawn times since the last read, in milliseconds.
#[wasm_bindgen]
pub fn take_latencies() -> Vec<f64> {
    std::mem::take(&mut *LATENCIES.lock().unwrap())
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
