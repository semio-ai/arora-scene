//! The browser host for the scene: the wasm module `@semio/scene` loads.
//!
//! A page calls [`mount`] once with a canvas, which starts an empty scene,
//! then loads robots into it with [`load`], each under an instance id the page
//! chooses, drives them with [`set_value`] and [`control`], and hears from the
//! scene through the handler it gives [`set_event_handler`]. Everything about
//! a robot names its instance.
//!
//! **One scene per page, for the life of the page.** Bevy's windowing runs on
//! winit, and winit allows one event loop per page and never returns from it,
//! so a second `mount` is refused rather than attempted. A host that shows and
//! hides the scene keeps its canvas mounted and hides that.
//!
//! Each GLB is handed over as bytes and served to Bevy's asset server from
//! memory, so the page decides where a robot comes from (a URL, an upload, a
//! database) and the scene never fetches anything itself.

use std::cell::RefCell;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use arora_scene::controls::Pose;
use arora_scene::controls::{
    ControlCommand, ControlCommands, ControlEvent, ControlEvents, ControlPress, ControlPresses,
};
use arora_scene::redraw::RedrawOnChangePlugin;
use arora_scene::robots::{Outcome, RobotLoad, RobotRequest, RobotRequests};
use arora_scene::stage::{Environment, Theme};
use arora_scene::values::{ApplyValues, ValueWrite, ValueWrites};
use arora_scene::{SceneOptions, ScenePlugin};
use arora_visuals::active_area::ActiveArea;
use arora_visuals::meta::FeatureKind;
use arora_visuals::overlay::Overlay;
use arora_visuals::pick::Picked;
use arora_visuals::select::Selection;
use arora_visuals::value::Decoded;
use bevy::asset::io::memory::{Dir, MemoryAssetReader};
use bevy::asset::io::AssetSourceBuilder;
use bevy::asset::AssetMetaCheck;
use bevy::prelude::*;
use bevy::window::WindowResolution;
use bevy::winit::{EventLoopProxyWrapper, WinitUserEvent};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

#[cfg(feature = "bench")]
mod bench;

/// The asset source robots are served from.
const SOURCE: &str = "memory";

/// The memory robots are served from. A robot's bytes stay until its load has
/// ended, either way: removed while the asset server is still reading them,
/// they fail the load they were for. Once it has loaded, the asset server
/// holds what it read.
static ROBOTS: Mutex<Option<Dir>> = Mutex::new(None);
/// How many robots have been loaded, which names each new one: a fresh path
/// per load, so the asset server never answers with one it held before.
static LOADED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// What the page asked of the robots since the last frame, in order.
static REQUESTS: Mutex<Vec<Request>> = Mutex::new(Vec::new());

enum Request {
    Robot(RobotRequest),
    Frame(Vec<String>),
    Theme(Theme),
    Environment(Environment),
    Area(Option<Rect>),
}

/// How a load is asked for, as JSON from the page. Every field is optional.
#[derive(Deserialize, Default)]
#[serde(default)]
struct LoadOptions {
    /// The page's name for the content, handed back in the load's events.
    /// Deciding that a load of what an instance already shows is a repeat is
    /// the package's, which compares keys before it calls.
    key: Option<String>,
    /// Where the robot stands, Z up, in metres. Absent, it stands where the
    /// robot it replaces stood, or at the origin.
    pose: Option<Pose>,
}

/// Loads the robot in `glb` into the instance `instance`, the page's own name
/// for it, with `options` as JSON (`{ key, pose }`). Under a new id it adds a
/// robot; under one the scene shows it swaps that robot, which keeps showing
/// until this one has loaded whole and then goes, with the controls on its
/// joints and elements. A `loaded` or `loadFailed` event naming the instance
/// says how it ended. Refused at once if `glb` declares nothing the scene can
/// read.
#[wasm_bindgen]
pub fn load(instance: String, glb: Vec<u8>, options: String) -> Result<(), JsValue> {
    let options: LoadOptions = match options.as_str() {
        "" => LoadOptions::default(),
        json => serde_json::from_str(json).map_err(error)?,
    };
    let robots = ROBOTS.lock().unwrap();
    let Some(dir) = robots.as_ref() else {
        return Err(error("the scene is not mounted yet"));
    };
    let path = format!("robot-{}.glb", LOADED.fetch_add(1, Ordering::SeqCst) + 1);
    let mut load = RobotLoad::read(instance, options.key, asset_id(&path), &glb)
        .map_err(|e| error(format!("{e:#}")))?;
    load.pose = options.pose;
    dir.insert_asset(Path::new(&path), glb);
    REQUESTS
        .lock()
        .unwrap()
        .push(Request::Robot(RobotRequest::Load(load)));
    wake();
    Ok(())
}

/// Takes the robot in the instance `instance` away, with the controls on it
/// and anything still loading into it.
#[wasm_bindgen]
pub fn unload(instance: String) {
    REQUESTS
        .lock()
        .unwrap()
        .push(Request::Robot(RobotRequest::Unload(instance)));
    wake();
}

/// Moves the robot in the instance `instance` to `pose`, as JSON
/// (`{ position, rotation }`, Z up, in metres, the rotation a quaternion
/// `[x, y, z, w]`).
#[wasm_bindgen]
pub fn set_pose(instance: String, pose: String) -> Result<(), JsValue> {
    let pose: Pose = serde_json::from_str(&pose).map_err(error)?;
    REQUESTS
        .lock()
        .unwrap()
        .push(Request::Robot(RobotRequest::Pose(instance, pose)));
    wake();
    Ok(())
}

/// Outlines the elements `elements` of the instance `instance`, by their
/// `RobotData` ids, in place of whatever it outlined before; none clears it.
/// A selection is the page's and is kept, by the instance, across a swap.
#[wasm_bindgen]
pub fn set_selection(instance: String, elements: Vec<String>) {
    let elements = elements.iter().map(Selection::of).collect();
    REQUESTS
        .lock()
        .unwrap()
        .push(Request::Robot(RobotRequest::Select(instance, elements)));
    wake();
}

/// Moves the camera to fit the robots in `instances`, or every robot for
/// none, as it fits the robots present when the scene first has robots.
#[wasm_bindgen]
pub fn frame(instances: Vec<String>) {
    REQUESTS.lock().unwrap().push(Request::Frame(instances));
    wake();
}

/// Sets the theme: `"dark"` or `"light"`.
#[wasm_bindgen]
pub fn set_theme(theme: &str) -> Result<(), JsValue> {
    let theme = match theme {
        "dark" => Theme::Dark,
        "light" => Theme::Light,
        other => return Err(error(format!("unknown theme {other:?}"))),
    };
    REQUESTS.lock().unwrap().push(Request::Theme(theme));
    wake();
    Ok(())
}

/// Sets the environment: `"plain"`, the ground colour all the way up, or
/// `"sky"`, which under the dark theme shows the aurora.
#[wasm_bindgen]
pub fn set_environment(environment: &str) -> Result<(), JsValue> {
    let environment = match environment {
        "plain" => Environment::Plain,
        "sky" => Environment::Sky,
        other => return Err(error(format!("unknown environment {other:?}"))),
    };
    REQUESTS
        .lock()
        .unwrap()
        .push(Request::Environment(environment));
    wake();
    Ok(())
}

/// Sets the part of the canvas left visible beside the host's panels,
/// `[x, y, width, height]` in CSS pixels, which the camera frames the robots
/// in, easing there; none gives the whole canvas back.
#[wasm_bindgen]
pub fn set_active_area(area: Vec<f32>) -> Result<(), JsValue> {
    let area = match area.as_slice() {
        [] => None,
        [x, y, w, h] => Some(Rect::new(*x, *y, x + w, y + h)),
        _ => return Err(error("an active area is [x, y, width, height]")),
    };
    REQUESTS.lock().unwrap().push(Request::Area(area));
    wake();
    Ok(())
}

/// Lets go of the bytes a load no longer needs once it has ended.
fn release_bytes(outcome: &Outcome) {
    let robots = ROBOTS.lock().unwrap();
    let (Some(dir), Some(path)) = (robots.as_ref(), outcome.glb().strip_prefix(&asset_id("")))
    else {
        return;
    };
    dir.remove_asset(Path::new(path));
}

/// How the asset server names the robot served at `path`.
fn asset_id(path: &str) -> String {
    format!("{SOURCE}://{path}")
}

/// Hands what the page asked since the last frame to the scene, in order.
fn take_requests(
    mut requests: ResMut<RobotRequests>,
    mut stage: ResMut<arora_scene::stage::Stage>,
    cameras: Query<Entity, With<arora_scene::OrbitCamera>>,
    mut commands: Commands,
) {
    for request in std::mem::take(&mut *REQUESTS.lock().unwrap()) {
        match request {
            Request::Frame(instances) => requests.frame = Some(instances),
            Request::Theme(theme) => stage.theme = theme,
            Request::Environment(environment) => stage.environment = environment,
            Request::Area(area) => {
                for camera in &cameras {
                    match area {
                        Some(area) => commands.entity(camera).insert(ActiveArea(area)),
                        None => commands.entity(camera).remove::<ActiveArea>(),
                    };
                }
            }
            Request::Robot(request) => requests.robots.push(request),
        }
    }
}

/// How the scene starts, as JSON from the page. Every field is optional.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Options {
    /// Cast shadows. On unless turned off.
    shadows: Option<bool>,
    /// `"dark"` (the default) or `"light"`.
    theme: Option<String>,
    /// `"sky"` for the sky, which under the dark theme shows the aurora;
    /// anything else is the ground colour all the way up.
    environment: Option<String>,
    /// The part of the canvas left visible beside the host's panels,
    /// `[x, y, width, height]` in CSS pixels; the camera frames the robot in
    /// it. The whole canvas when absent.
    active_area: Option<[f32; 4]>,
    /// Render at this many physical pixels per CSS pixel instead of the
    /// page's `devicePixelRatio`.
    scale_factor: Option<f32>,
    /// The camera's starting height above the model, in radians; negative
    /// looks up from below its middle.
    pitch: Option<f32>,
    /// The camera's starting yaw about the model, in radians.
    angle: Option<f32>,
    #[cfg(feature = "bench")]
    #[serde(flatten)]
    bench: bench::Options,
}

pub(crate) fn now() -> f64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map(|p| p.now())
        .unwrap_or(0.0)
}

fn mark(_name: &str) {
    #[cfg(feature = "bench")]
    bench::mark(_name);
}

fn error(message: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&message.to_string())
}

static MOUNTED: AtomicBool = AtomicBool::new(false);

/// Starts an empty scene in the canvas whose CSS selector is `canvas`
/// (`#scene`), with `options` as JSON (see the package's `SceneOptions`
/// type). Robots come after, through [`load`].
#[wasm_bindgen]
pub fn mount(canvas: String, options: String) -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    if MOUNTED.load(Ordering::SeqCst) {
        return Err(error("the scene is already mounted; a page holds one"));
    }
    mark("start");
    let opts: Options = serde_json::from_str(&options).map_err(error)?;
    // Claimed only once the options are known good, so a mount refused for
    // bad options leaves the page free to mount again.
    if MOUNTED.swap(true, Ordering::SeqCst) {
        return Err(error("the scene is already mounted; a page holds one"));
    }

    let robots = Dir::default();
    *ROBOTS.lock().unwrap() = Some(robots.clone());

    let mut resolution = WindowResolution::default();
    if let Some(sf) = opts.scale_factor {
        resolution.set_scale_factor_override(Some(sf));
    }

    #[cfg(feature = "bench")]
    let (still, cpu_preprocess, gizmo) = (
        opts.bench.still,
        opts.bench.cpu_preprocess,
        opts.bench
            .gizmo
            .then_some(arora_visuals::transform_gizmo::TransformModes::ALL),
    );
    // A still camera: one that turns by itself asks for a frame every frame,
    // and a scene in a page has to go idle. Only the benchmark turns it, to
    // measure a moving view. Nor is there a gizmo of the scene's own: every
    // control on a page is a driver's, added through `control`.
    #[cfg(not(feature = "bench"))]
    let (still, cpu_preprocess, gizmo) = (true, false, None);

    let mut app = App::new();
    // Sources are built when the asset plugin is added, so this comes first.
    app.register_asset_source(
        SOURCE,
        AssetSourceBuilder::new(move || {
            Box::new(MemoryAssetReader {
                root: robots.clone(),
            })
        }),
    );
    app.add_plugins(
        DefaultPlugins
            .build()
            // Present because a dependency compiles code against it, not
            // because the scene uses it.
            .disable::<bevy::state::app::StatesPlugin>()
            .set(WindowPlugin {
                primary_window: Some(Window {
                    canvas: Some(canvas),
                    // Sized by its parent, which the host lays out.
                    fit_canvas_to_parent: true,
                    prevent_default_event_handling: true,
                    resolution,
                    ..default()
                }),
                ..default()
            })
            .set(AssetPlugin {
                // Nothing is fetched from the default source, and no `.meta`
                // files exist to fetch.
                meta_check: AssetMetaCheck::Never,
                ..default()
            })
            .set(bevy::pbr::PbrPlugin {
                use_gpu_instance_buffer_builder: !cpu_preprocess,
                ..default()
            }),
    )
    .add_plugins(ScenePlugin)
    .insert_resource(SceneOptions {
        still,
        shadows: opts.shadows.unwrap_or(true),
        gizmo,
        theme: match opts.theme.as_deref() {
            Some("light") => Theme::Light,
            _ => Theme::Dark,
        },
        environment: match opts.environment.as_deref() {
            Some("sky") => Environment::Sky,
            _ => Environment::Plain,
        },
        pitch: opts.pitch.unwrap_or(SceneOptions::default().pitch),
        angle: opts.angle.unwrap_or(SceneOptions::default().angle),
        ..default()
    })
    .add_systems(Startup, keep_waker)
    .add_systems(Update, take_pushed.before(ApplyValues))
    // Before Update, where the scene carries control commands out.
    .add_systems(PreUpdate, take_controls)
    // Before the scene takes its robot requests in PreUpdate, so a robot
    // asked for between frames starts loading on the next one.
    .add_systems(First, take_requests)
    // Poses first, so a handler reading one in an event's handler reads
    // the frame the event is about.
    .add_systems(Last, (keep_poses, emit_events).chain());

    #[cfg(feature = "bench")]
    let continuous = bench::install(&mut app, &opts.bench);
    #[cfg(not(feature = "bench"))]
    let continuous = false;
    if !continuous {
        app.add_plugins(RedrawOnChangePlugin);
    }
    if let Some([x, y, w, h]) = opts.active_area {
        REQUESTS
            .lock()
            .unwrap()
            .push(Request::Area(Some(Rect::new(x, y, x + w, y + h))));
    }
    mark("app-built");
    app.run();
    Ok(())
}

thread_local! {
    /// Wakes the event loop from outside it. The page calls in between
    /// frames, when a loop that draws only on change may be waiting
    /// indefinitely.
    static WAKE: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
    /// Whether a wake is already on its way, so that every call the page
    /// makes before it arrives shares it.
    static WAKE_PENDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// What the browser runs to deliver a wake, kept for the page's life so
    /// each wake does not allocate a closure.
    static WAKE_LATER: Closure<dyn FnMut()> = Closure::new(|| {
        WAKE_PENDING.with(|pending| pending.set(false));
        wake_now();
    });
    /// Where the scene's events go: the page's handler.
    static HANDLER: RefCell<Option<js_sys::Function>> = const { RefCell::new(None) };
}

/// Wakes the event loop once the page's current task is over, once however
/// many calls the page makes in it.
///
/// Not at once: winit cancels the animation frame it has requested and asks
/// for the next one whenever it is woken, and runs no further update until
/// that frame arrives. A page writing from inside its own animation frames,
/// as playback does, would wake it in every frame before winit's own frame
/// callback ran, putting the redraw off for as long as the writes continued.
/// A timeout runs after the browser's animation frame callbacks, so winit's
/// frame is drawn first and the wake asks for the next.
fn wake() {
    if WAKE_PENDING.with(|pending| pending.replace(true)) {
        return;
    }
    let scheduled = WAKE_LATER.with(|later| {
        web_sys::window().and_then(|window| {
            window
                .set_timeout_with_callback(later.as_ref().unchecked_ref())
                .ok()
        })
    });
    if scheduled.is_none() {
        WAKE_PENDING.with(|pending| pending.set(false));
        wake_now();
    }
}

fn wake_now() {
    WAKE.with(|wake| {
        if let Some(wake) = wake.borrow().as_ref() {
            wake();
        }
    });
}

fn keep_waker(proxy: Res<EventLoopProxyWrapper>) {
    let proxy = (**proxy).clone();
    WAKE.with(|wake| {
        *wake.borrow_mut() = Some(Box::new(move || {
            let _ = proxy.send_event(WinitUserEvent::WakeUp);
        }));
    });
}

/// Writes waiting for the next frame, each stamped with when it was written.
static PUSHED: Mutex<Vec<ValueWrite>> = Mutex::new(Vec::new());

/// Hands the page's writes to the scene's one place for applied values.
fn take_pushed(mut writes: ResMut<ValueWrites>) {
    writes.0.append(&mut PUSHED.lock().unwrap());
}

/// Writes one value, one number or three, to `target` on the robot in the
/// instance `instance`: an animatable's id as the robot's `RobotData` declares
/// it, which carries its own feature (a joint's value among them), or an
/// element's id with the `feature` to change as `RobotData` names it
/// (`rotation`, `translation`, `color`, ...). An empty `feature` writes an
/// animatable. Takes effect in the next frame, which the call wakes the scene
/// to draw, or, for a robot still loading, in the frame it arrives in.
#[wasm_bindgen]
pub fn set_value(
    instance: String,
    target: String,
    feature: String,
    values: Vec<f32>,
) -> Result<(), JsValue> {
    let feature = match feature.as_str() {
        "" => None,
        name => Some(
            FeatureKind::from_name(name, &[])
                .ok_or_else(|| error(format!("unknown feature {feature:?}")))?,
        ),
    };
    let value = match values.as_slice() {
        [s] => Decoded::Scalar(*s),
        [x, y, z] => Decoded::Triple([*x, *y, *z]),
        _ => return Err(error("a value is one number or three")),
    };
    PUSHED.lock().unwrap().push(ValueWrite {
        instance: Some(instance),
        target,
        feature,
        value,
        // When it was written, which the scene hands back once it lands.
        stamp: Some(now()),
    });
    wake();
    Ok(())
}

/// Sets the function the scene's events go to, replacing any earlier one.
/// Each call gets one event object; its `type` says which (see the package's
/// `SceneEvent` type).
#[wasm_bindgen]
pub fn set_event_handler(handler: js_sys::Function) {
    HANDLER.with(|h| *h.borrow_mut() = Some(handler));
    // A new handler starts with the overlay as it stands, which an idle scene
    // would otherwise not send until something on screen moved.
    SNAPSHOT.store(true, Ordering::SeqCst);
    wake();
}

/// Whether the next frame sends the overlay even though it did not change.
static SNAPSHOT: AtomicBool = AtomicBool::new(false);

/// An event for the page, serialised as the object its handler receives.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum SceneEvent<'a> {
    /// What the page needs to draw labels and tooltips over the scene this
    /// frame: element and gizmo boxes on screen, and the gizmo the pointer
    /// has.
    Overlay(&'a arora_visuals::overlay::OverlayFrame),
    /// A press on the robot: the element under it, or `null` for a press on
    /// the robot that hit no element.
    Pick {
        instance: &'a str,
        element: Option<&'a str>,
    },
    /// A drag on a control: what it asks for. Nothing has moved; the page
    /// writes the change if it wants it.
    Control(&'a ControlEvent),
    /// A robot loaded whole and showing in its instance, with what it
    /// declares.
    Loaded {
        instance: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        key: Option<&'a str>,
        joints: Vec<JointInfo<'a>>,
        elements: Vec<ElementInfo<'a>>,
    },
    /// A robot showing in its instance has been drawn whole: every mesh of
    /// it in the camera's view with its pipeline compiled, and the frame
    /// done.
    Ready { instance: &'a str },
    /// A load that did not finish, and why: the GLB failed, a later load into
    /// the same instance replaced it, or the instance was unloaded. Whatever
    /// the instance showed before still shows.
    LoadFailed {
        instance: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        key: Option<&'a str>,
        error: &'a str,
    },
    /// A press with a button other than the primary on a control, which the
    /// page acts on as it likes.
    ControlPress(&'a ControlPress),
}

/// An element as the page sees it: its id, which writes, poses and picks
/// name it by, and what its author called it.
#[derive(Serialize)]
struct ElementInfo<'a> {
    id: &'a str,
    name: &'a str,
}

/// A joint as the page sees it: what to call it, what to write to move it,
/// and how far it goes.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JointInfo<'a> {
    name: &'a str,
    kind: &'static str,
    /// The element the joint moves, by its `RobotData` id.
    element: &'a str,
    /// The animatable to write to move it, absent for a joint that is not
    /// animated. Joints that mimic one another share their leader's.
    #[serde(skip_serializing_if = "Option::is_none")]
    animatable: Option<&'a str>,
    /// Present on a joint that follows another: it stands at
    /// `value * multiplier + offset` of the animatable's value.
    #[serde(skip_serializing_if = "Option::is_none")]
    mimic: Option<Mimic>,
    /// The limits, absent where the joint has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    min: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max: Option<f32>,
    default: f32,
}

#[derive(Serialize)]
struct Mimic {
    multiplier: f32,
    offset: f32,
}

impl<'a> From<&'a arora_scene::robot::Joint> for JointInfo<'a> {
    fn from(joint: &'a arora_scene::robot::Joint) -> Self {
        use arora_scene::robot::JointKind;
        Self {
            name: &joint.name,
            kind: match joint.kind {
                JointKind::Revolute => "revolute",
                JointKind::Continuous => "continuous",
                JointKind::Prismatic => "prismatic",
                JointKind::Fixed => "fixed",
            },
            element: &joint.child,
            animatable: joint.animatable.as_deref(),
            mimic: joint
                .mimic
                .map(|(multiplier, offset)| Mimic { multiplier, offset }),
            min: joint.min.is_finite().then_some(joint.min),
            max: joint.max.is_finite().then_some(joint.max),
            default: joint.default,
        }
    }
}

/// Where each element of each robot showing stood in the last frame, by its
/// instance and `RobotData` id, Z up.
type Poses = std::collections::HashMap<(String, String), Pose>;

thread_local! {
    static POSES: RefCell<Poses> = RefCell::new(Poses::new());
}

/// Records where each element of each robot showing stands, for
/// [`element_pose`]. Runs after transforms have propagated, so it reads the
/// frame about to be drawn.
fn keep_poses(
    instances: Query<
        &arora_visuals::instance::EntityInstance,
        Without<arora_scene::robots::Incoming>,
    >,
    placed: Query<&GlobalTransform>,
) {
    let mut poses = Poses::new();
    for instance in &instances {
        for (id, targets) in &instance.bindings.elements {
            if let Ok(placed) = placed.get(targets.node) {
                poses.insert(
                    (instance.id.clone(), id.clone()),
                    Pose::from_scene(&placed.compute_transform()),
                );
            }
        }
    }
    POSES.with(|kept| *kept.borrow_mut() = poses);
}

/// Where an element of the robot in the instance `instance` stood in the last
/// frame drawn, in the scene's world, Z up: `{ position, rotation }`, the
/// rotation a quaternion `[x, y, z, w]`. `undefined` for an id the robot does
/// not declare, or before it has spawned.
#[wasm_bindgen]
pub fn element_pose(instance: &str, element: &str) -> JsValue {
    POSES.with(|poses| {
        poses
            .borrow()
            .get(&(instance.to_string(), element.to_string()))
            .and_then(|pose| serde_json::to_string(pose).ok())
            .and_then(|json| js_sys::JSON::parse(&json).ok())
            .unwrap_or(JsValue::UNDEFINED)
    })
}

/// Controls waiting for the next frame.
static CONTROLS: Mutex<Vec<ControlCommand>> = Mutex::new(Vec::new());

/// Adds, removes, shows, hides or moves a control, by a command as JSON (see
/// the package's `ControlCommand` type). Takes effect in the next frame, or,
/// for a control on a robot still loading, in the frame it arrives in.
#[wasm_bindgen]
pub fn control(command: String) -> Result<(), JsValue> {
    let command: ControlCommand = serde_json::from_str(&command).map_err(error)?;
    CONTROLS.lock().unwrap().push(command);
    wake();
    Ok(())
}

/// Hands the page's control commands to the scene.
fn take_controls(mut commands: ResMut<ControlCommands>) {
    commands.0.append(&mut CONTROLS.lock().unwrap());
}

fn send(event: &SceneEvent) {
    HANDLER.with(|h| {
        let Some(handler) = h.borrow().clone() else {
            return;
        };
        let Ok(json) = serde_json::to_string(event) else {
            return;
        };
        let Ok(value) = js_sys::JSON::parse(&json) else {
            return;
        };
        // A throwing handler is the page's error; it must not stop the frame.
        if let Err(e) = handler.call1(&JsValue::NULL, &value) {
            web_sys::console::error_1(&e);
        }
    });
}

/// Sends the frame's overlay when it changed, and every pick on a robot since
/// the last frame. Picks are read either way, so a page that sets its handler
/// late does not get a backlog; a pick on another consumer's instance sharing
/// the app is that consumer's.
#[allow(clippy::too_many_arguments)]
fn emit_events(
    overlay: Res<Overlay>,
    mut picks: MessageReader<Picked>,
    rigged: Query<(), With<arora_scene::joints::JointRig>>,
    mut controls: ResMut<ControlEvents>,
    mut presses: ResMut<ControlPresses>,
    mut robots: ResMut<arora_scene::robots::RobotOutcomes>,
    mut drawn: ResMut<arora_scene::ready::RobotsReady>,
) {
    for outcome in std::mem::take(&mut robots.0) {
        release_bytes(&outcome);
        send(&match &outcome {
            Outcome::Loaded {
                instance,
                key,
                joints,
                elements,
                ..
            } => SceneEvent::Loaded {
                instance,
                key: key.as_deref(),
                joints: joints.iter().map(JointInfo::from).collect(),
                elements: elements
                    .iter()
                    .map(|(id, name)| ElementInfo { id, name })
                    .collect(),
            },
            Outcome::Failed {
                instance,
                key,
                error,
                ..
            } => SceneEvent::LoadFailed {
                instance,
                key: key.as_deref(),
                error,
            },
        });
    }
    for instance in std::mem::take(&mut drawn.0) {
        send(&SceneEvent::Ready {
            instance: &instance,
        });
    }
    for event in std::mem::take(&mut controls.0) {
        send(&SceneEvent::Control(&event));
    }
    for press in std::mem::take(&mut presses.0) {
        send(&SceneEvent::ControlPress(&press));
    }
    let picks: Vec<Picked> = picks
        .read()
        .filter(|pick| rigged.contains(pick.instance))
        .cloned()
        .collect();
    if SNAPSHOT.swap(false, Ordering::SeqCst) || overlay.is_changed() {
        send(&SceneEvent::Overlay(&overlay.0));
    }
    for pick in &picks {
        send(&SceneEvent::Pick {
            instance: &pick.instance_id,
            element: pick.element_id.as_deref(),
        });
    }
}
