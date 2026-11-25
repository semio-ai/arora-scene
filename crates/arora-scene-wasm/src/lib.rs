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

use arora_scene::controls::{
    ControlCommand, ControlCommands, ControlEvent, ControlEvents, ControlPress, ControlPresses,
    GizmoDrags, Modifiers,
};
use arora_scene::controls::{Pose, ScenePose};
use arora_scene::redraw::RedrawOnChangePlugin;
use arora_scene::robots::{Outcome, RobotLoad, RobotRequest, RobotRequests};
use arora_scene::stage::{BelowFloor, Environment, Theme};
use arora_scene::values::{ApplyValues, ValueWrite, ValueWrites};
use arora_scene::{SceneOptions, ScenePlugin};
use arora_visuals::active_area::ActiveArea;
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

/// The memory robots are served from, once the scene is mounted.
static ROBOTS: Mutex<Option<Served>> = Mutex::new(None);

/// Robots' bytes, served by path, with how many loads still need each.
///
/// A path's bytes stay while any load of them has yet to end, either way:
/// removed while the asset server is still reading them, they fail the load
/// they were for. Loads of the same bytes share a path ([`content_path`]),
/// and the asset server reads after the frame a load is asked in, so one load
/// ending — replaced, unloaded, or loaded — must not take the bytes from
/// another of the same robot still waiting to be read. Once no load needs
/// them they go: a robot that has loaded is held by the asset server, and a
/// later load puts its bytes back before it is asked for.
#[derive(Clone, Default)]
struct Served {
    dir: Dir,
    loads: std::collections::HashMap<String, usize>,
}

impl Served {
    /// Serves `bytes` at `path` for one more load.
    fn serve(&mut self, path: &str, bytes: Vec<u8>) {
        self.dir.insert_asset(Path::new(path), bytes);
        *self.loads.entry(path.to_string()).or_default() += 1;
    }

    /// One load of `path` has ended; its bytes go once no load needs them.
    fn release(&mut self, path: &str) {
        let Some(count) = self.loads.get_mut(path) else {
            return;
        };
        *count -= 1;
        if *count == 0 {
            self.loads.remove(path);
            self.dir.remove_asset(Path::new(path));
        }
    }
}

/// What the page asked of the robots since the last frame, in order.
static REQUESTS: Mutex<Vec<Request>> = Mutex::new(Vec::new());

enum Request {
    Robot(RobotRequest),
    Frame(Vec<String>),
    Theme(Theme),
    Environment(Environment),
    BelowFloor(BelowFloor),
    Area(Option<Rect>),
    CancelDrag,
    Modifiers(Modifiers),
}

/// What the floor shows of whatever has sunk below it, as JSON from the page.
#[derive(Deserialize, Default)]
#[serde(default)]
struct BelowFloorOptions {
    /// A CSS hex colour; the scene's warning colour when absent.
    color: Option<String>,
    /// How far below the floor, in metres, a surface may go unshown.
    tolerance: Option<f32>,
}

impl BelowFloorOptions {
    /// The floor's setting, shown.
    fn shown(self) -> Result<BelowFloor, JsValue> {
        let mut below = BelowFloor {
            show: true,
            ..default()
        };
        if let Some(colour) = self.color {
            below.colour = Srgba::hex(&colour)
                .map_err(|e| error(format!("{colour:?} is not a colour: {e}")))?
                .into();
        }
        if let Some(tolerance) = self.tolerance {
            if !(tolerance.is_finite() && tolerance >= 0.0) {
                return Err(error("a tolerance is a distance of zero or more"));
            }
            below.tolerance = tolerance;
        }
        Ok(below)
    }
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
    let mut robots = ROBOTS.lock().unwrap();
    let Some(served) = robots.as_mut() else {
        return Err(error("the scene is not mounted yet"));
    };
    let path = content_path(&glb);
    let mut load = RobotLoad::read(instance, options.key, asset_id(&path), &glb)
        .map_err(|e| error(format!("{e:#}")))?;
    load.pose = options.pose;
    served.serve(&path, glb);
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

/// Draws the robot in the instance `instance` as a ghost, with `options` as
/// JSON (`{ color, wash, rim, opacity, depth }`, all optional), or takes
/// the page's ghost off for none: its style goes, and the ghost too unless
/// the robot was one before the page asked. Kept, by the instance, across a
/// swap; asked again with a
/// new style, the ghost changes from the next frame, so a page fades one by
/// asking for it with its opacity falling.
#[wasm_bindgen]
pub fn set_ghost(instance: String, options: String) -> Result<(), JsValue> {
    let style = match options.as_str() {
        "" => None,
        json => Some(
            serde_json::from_str::<arora_scene::ghosts::GhostOptions>(json)
                .map_err(error)?
                .style()
                .map_err(error)?,
        ),
    };
    REQUESTS
        .lock()
        .unwrap()
        .push(Request::Robot(RobotRequest::Ghost(instance, style)));
    wake();
    Ok(())
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

/// Shows what has sunk below the floor through it, with `options` as JSON
/// (`color`, `tolerance`), or stops showing it for an empty string.
#[wasm_bindgen]
pub fn set_below_floor(options: &str) -> Result<(), JsValue> {
    let below = if options.is_empty() {
        BelowFloor::default()
    } else {
        serde_json::from_str::<BelowFloorOptions>(options)
            .map_err(error)?
            .shown()?
    };
    REQUESTS.lock().unwrap().push(Request::BelowFloor(below));
    wake();
    Ok(())
}

/// Sets the part of the canvas left visible beside the host's panels,
/// `[x, y, width, height]` in CSS pixels, which the camera frames the robots
/// in, easing there from an area set before; the first is taken at once,
/// and none gives the whole canvas back at once.
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

/// Where a robot's bytes are served from, named by what they hold. A load of
/// bytes another instance already shows is then answered with the robot the
/// asset server holds, its meshes and materials shared rather than read and
/// uploaded again; different bytes never share a path, so a robot edited
/// under the same key is read afresh. The content decides rather than the
/// host's key for that reason: a key names what a host means to show, not
/// what the bytes are.
fn content_path(glb: &[u8]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    glb.hash(&mut hasher);
    format!("robot-{:016x}-{}.glb", hasher.finish(), glb.len())
}

/// Lets go of the bytes a load no longer needs once it has ended.
fn release_bytes(outcome: &Outcome) {
    let mut robots = ROBOTS.lock().unwrap();
    let (Some(served), Some(path)) = (robots.as_mut(), outcome.glb().strip_prefix(&asset_id("")))
    else {
        return;
    };
    served.release(path);
}

/// How the asset server names the robot served at `path`.
fn asset_id(path: &str) -> String {
    format!("{SOURCE}://{path}")
}

/// Hands what the page asked since the last frame to the scene, in order.
#[allow(clippy::too_many_arguments)]
fn take_requests(
    mut requests: ResMut<RobotRequests>,
    mut stage: ResMut<arora_scene::stage::Stage>,
    mut below: ResMut<BelowFloor>,
    mut drags: ResMut<GizmoDrags>,
    mut modifiers: ResMut<Modifiers>,
    cameras: Query<Entity, With<arora_scene::OrbitCamera>>,
    mut commands: Commands,
) {
    for request in std::mem::take(&mut *REQUESTS.lock().unwrap()) {
        match request {
            Request::Frame(instances) => requests.frame = Some(instances),
            Request::Theme(theme) => stage.theme = theme,
            Request::Environment(environment) => stage.environment = environment,
            Request::BelowFloor(setting) => {
                if *below != setting {
                    *below = setting;
                }
            }
            Request::Area(area) => {
                for camera in &cameras {
                    match area {
                        Some(area) => commands.entity(camera).insert(ActiveArea(area)),
                        None => commands.entity(camera).remove::<ActiveArea>(),
                    };
                }
            }
            Request::Robot(request) => requests.robots.push(request),
            Request::CancelDrag => drags.cancel_drag(),
            Request::Modifiers(held) => {
                modifiers.set_if_neq(held);
            }
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
    /// Show what has sunk below the floor through it. Not shown when absent.
    below_floor: Option<BelowFloorOptions>,
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
    let mut opts: Options = serde_json::from_str(&options).map_err(error)?;
    let below = match opts.below_floor.take() {
        Some(below) => below.shown()?,
        None => BelowFloor::default(),
    };
    // Claimed only once the options are known good, so a mount refused for
    // bad options leaves the page free to mount again.
    if MOUNTED.swap(true, Ordering::SeqCst) {
        return Err(error("the scene is already mounted; a page holds one"));
    }

    let robots = Dir::default();
    *ROBOTS.lock().unwrap() = Some(Served {
        dir: robots.clone(),
        ..default()
    });

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
    .insert_resource(below)
    .add_systems(Startup, keep_waker)
    .add_systems(
        Update,
        take_pushed
            .in_set(arora_visuals::instance::InstanceSystems::Intake)
            .before(ApplyValues),
    )
    // Before Update, where the scene carries control commands out, and after
    // the robot requests, so an unload drops only commands that were waiting
    // from earlier frames.
    .add_systems(
        PreUpdate,
        take_controls.after(arora_scene::robots::RobotRequestSystems),
    )
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
    // Off the web there is no page loop to wake: only this crate's tests
    // build it natively, and they run each frame themselves.
    if cfg!(not(target_arch = "wasm32")) {
        return;
    }
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
/// (`rotation`, `translation`, `color`, a per-axis `translation.x`, one of
/// the element's morph target names, ...). An empty `feature` writes an
/// animatable. A feature the element does not have, or a value of the wrong
/// shape for it, is said in the log and changes nothing. Takes effect in the next frame, which the call wakes the scene
/// to draw, or, for a robot still loading, in the frame it arrives in.
#[wasm_bindgen]
pub fn set_value(
    instance: String,
    target: String,
    feature: String,
    values: Vec<f32>,
) -> Result<(), JsValue> {
    // Resolved against the element it lands on, which alone knows its morph
    // targets.
    let feature = (!feature.is_empty()).then_some(feature);
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
    /// writes the change if it wants it. Each drag closes exactly once, with
    /// `end` or `cancel`, and every event of it carries its `gesture` and the
    /// modifiers the page last said were held ([`set_modifiers`]).
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
    /// A robot showing in its instance has been drawn as it will be: every
    /// mesh of it in the camera's view with its pipeline compiled (or failed
    /// for good, which the renderer logs), and the frame done; or, `drawn`
    /// false, the scene gave up waiting to see that.
    Ready { instance: &'a str, drawn: bool },
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
    let command = arora_scene::controls::read_command(&command).map_err(error)?;
    CONTROLS.lock().unwrap().push(command);
    wake();
    Ok(())
}

/// Abandons the drag in hand on a control, if there is one: it closes with a
/// `cancel` event, and nothing more is sent for it, the button coming up
/// included. For what the scene cannot see itself — Escape, the window
/// losing focus, a cancelled pointer, the page hidden. Takes effect in the
/// next frame, which the call wakes the scene for, before the gizmos handle
/// the pointer in it.
#[wasm_bindgen]
pub fn cancel_drag() {
    REQUESTS.lock().unwrap().push(Request::CancelDrag);
    wake();
}

/// Says which modifier keys are held, for the scene to stamp on each control
/// event from the next frame on. The page reads them from its own pointer and
/// key events, since the canvas sees no keys while it lacks focus.
///
/// Does not wake the scene: what is held changes nothing drawn and sends
/// nothing by itself, and the pointer event a page reads them from wakes the
/// scene on its own. So a page may call this many times while the scene is
/// idle and takes no requests, and only the last state can reach an event:
/// it replaces any state still waiting rather than queueing behind it.
/// Moving it past the requests between is harmless, since none of them reads
/// what is held; events read it only once the frame's requests are taken.
#[wasm_bindgen]
pub fn set_modifiers(alt: bool, shift: bool, meta: bool, ctrl: bool) {
    let mut requests = REQUESTS.lock().unwrap();
    requests.retain(|request| !matches!(request, Request::Modifiers(_)));
    requests.push(Request::Modifiers(Modifiers {
        alt,
        shift,
        meta,
        ctrl,
    }));
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
    for ready in std::mem::take(&mut drawn.0) {
        send(&SceneEvent::Ready {
            instance: &ready.instance,
            drawn: ready.drawn,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A load ending — replaced, unloaded or loaded — leaves the bytes of
    /// the same robot for another load still waiting to read them, and the
    /// bytes go once the last load of them ends.
    #[test]
    fn bytes_stay_while_any_load_of_them_waits() {
        let mut served = Served::default();
        let robot = vec![1u8, 2, 3];
        let path = content_path(&robot);
        let there = |served: &Served| served.dir.get_asset(Path::new(&path)).is_some();
        // `load("a", X)`, `load("b", X)`, then a's load ends before the
        // asset server has read either.
        served.serve(&path, robot.clone());
        served.serve(&path, robot.clone());
        served.release(&path);
        assert!(there(&served), "b's load still needs them");
        served.release(&path);
        assert!(!there(&served), "no load needs them");
        // An outcome for bytes no load is serving changes nothing.
        served.release(&path);
        served.serve(&path, robot);
        assert!(there(&served), "a later load puts them back");
    }

    /// Loads of the same bytes share one path, so the asset server answers
    /// the second with the robot it holds; different bytes never share one.
    #[test]
    fn the_same_bytes_are_served_from_one_path() {
        let robot = vec![1u8, 2, 3, 4];
        assert_eq!(content_path(&robot), content_path(&robot.clone()));
        assert_ne!(content_path(&robot), content_path(&[1, 2, 3, 5]));
        assert_ne!(content_path(&robot), content_path(&[1, 2, 3]));
    }

    mod drags {
        use std::sync::MutexGuard;

        use arora_scene::OrbitCamera;
        use arora_visuals::transform_gizmo::{
            GizmoHandle, GizmoPointer, HandleMaterial, TransformGizmo, TransformGizmoPlugin,
        };
        use bevy::camera::visibility::VisibilityPlugin;
        use bevy::camera::{
            CameraProjection, ComputedCameraValues, NormalizedRenderTarget, RenderTarget,
            RenderTargetInfo,
        };
        use bevy::picking::backend::HitData;
        use bevy::picking::events::{Pointer, PointerPress};
        use bevy::picking::pointer::{Location, PointerButton, PointerId};
        use bevy::window::{RequestRedraw, WindowRef};
        use serde_json::{json, Value};

        use super::super::*;

        /// The page's queues are the module's, one each, so tests that go
        /// through them take turns.
        static PAGE: Mutex<()> = Mutex::new(());

        const KEY: &str = "goal";

        /// The page's side of a scene: the handle's own intake of requests
        /// and control commands, the scene's controls and transform gizmos,
        /// and a window and camera for a pointer to drag in. The camera
        /// draws 400 pixels a side, looking at the origin, where the page's
        /// goal control stands. The left button is pressed and let go by
        /// hand, with nothing clearing it between frames but the test.
        struct Page {
            app: App,
            window: Entity,
            /// Where on the window a handle of the goal is, once found.
            spot: Vec2,
            _turn: MutexGuard<'static, ()>,
        }

        impl Page {
            fn new() -> Self {
                let turn = PAGE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                REQUESTS.lock().unwrap().clear();
                CONTROLS.lock().unwrap().clear();
                let mut app = App::new();
                app.add_plugins((
                    MinimalPlugins,
                    AssetPlugin::default(),
                    bevy::transform::TransformPlugin,
                    VisibilityPlugin,
                ))
                // What the gizmos' handles are drawn with, and what visibility
                // reads meshes' bounds from, which the render plugins would
                // otherwise provide.
                .init_asset::<Mesh>()
                .init_asset::<bevy::mesh::skinning::SkinnedMeshInverseBindposes>()
                .init_asset::<HandleMaterial>()
                .init_asset::<StandardMaterial>()
                .add_message::<RequestRedraw>()
                .init_resource::<ButtonInput<MouseButton>>()
                .init_resource::<RobotRequests>()
                .init_resource::<BelowFloor>()
                .insert_resource(arora_scene::stage::Stage {
                    theme: default(),
                    environment: default(),
                    lighting: default(),
                })
                .add_plugins((arora_scene::controls::ControlsPlugin, TransformGizmoPlugin))
                .add_systems(First, take_requests)
                .add_systems(PreUpdate, take_controls);
                let world = app.world_mut();
                let window = world.spawn(Window::default()).id();
                world.spawn((
                    OrbitCamera,
                    Camera {
                        computed: ComputedCameraValues {
                            clip_from_view: PerspectiveProjection {
                                aspect_ratio: 1.0,
                                ..default()
                            }
                            .get_clip_from_view(),
                            target_info: Some(RenderTargetInfo {
                                physical_size: UVec2::splat(400),
                                scale_factor: 1.0,
                            }),
                            ..default()
                        },
                        ..default()
                    },
                    Transform::from_xyz(0.0, 0.4, 1.5).looking_at(Vec3::ZERO, Vec3::Y),
                    RenderTarget::Window(WindowRef::Entity(window)),
                ));
                let mut page = Self {
                    app,
                    window,
                    spot: Vec2::ZERO,
                    _turn: turn,
                };
                control(
                    json!({
                        "op": "add",
                        "key": KEY,
                        "control": {
                            "kind": "transform",
                            "target": { "kind": "goal", "pose": { "position": [0, 0, 0] } },
                        },
                    })
                    .to_string(),
                )
                .unwrap();
                page.app.update();
                page.app.update();
                // Along the row through the goal until a handle is under the
                // cursor; not a ring, which is seen edge on there and so
                // cannot be taken.
                page.spot = (0..200)
                    .map(|x| Vec2::new(200.0 + x as f32, 200.0))
                    .find(|&at| {
                        page.point_at(at);
                        let pointer = page.app.world().resource::<GizmoPointer>();
                        pointer
                            .hovered
                            .is_some_and(|(_, handle)| !matches!(handle, GizmoHandle::Ring(_)))
                    })
                    .expect("a handle of the goal on the row through it");
                page.sent();
                page
            }

            fn point_at(&mut self, at: Vec2) {
                let mut window = self.app.world_mut().get_mut::<Window>(self.window).unwrap();
                window.set_cursor_position(Some(at));
                self.app.update();
            }

            fn buttons(&mut self) -> Mut<'_, ButtonInput<MouseButton>> {
                self.app
                    .world_mut()
                    .resource_mut::<ButtonInput<MouseButton>>()
            }

            /// A left press on the scene where the cursor is, as winit and
            /// Bevy's picking would report one on the window.
            fn press(&mut self) {
                let camera = self
                    .app
                    .world_mut()
                    .query_filtered::<Entity, With<OrbitCamera>>()
                    .single(self.app.world())
                    .unwrap();
                let at = self.spot;
                self.app.world_mut().trigger(PointerPress {
                    entity: self.window,
                    pointer: Pointer::new(
                        PointerId::Mouse,
                        Location {
                            target: NormalizedRenderTarget::Window(
                                WindowRef::Entity(self.window).normalize(None).unwrap(),
                            ),
                            position: at,
                        },
                    ),
                    button: PointerButton::Primary,
                    hit: HitData::new(camera, 0.0, None, None),
                    count: 1,
                });
                self.buttons().press(MouseButton::Left);
                self.app.update();
                // Still down, no longer just pressed.
                self.buttons().clear();
            }

            fn release(&mut self) {
                self.buttons().release(MouseButton::Left);
                self.app.update();
                self.buttons().clear();
            }

            /// Takes a drag on the goal and moves it, so the drag has
            /// something to keep or drop.
            fn drag(&mut self) {
                self.press();
                self.point_at(self.spot + Vec2::new(6.0, -4.0));
            }

            /// The control events since last asked, as the page's handler
            /// receives them.
            fn sent(&mut self) -> Vec<Value> {
                std::mem::take(&mut self.app.world_mut().resource_mut::<ControlEvents>().0)
                    .iter()
                    .map(|event| serde_json::to_value(SceneEvent::Control(event)).unwrap())
                    .collect()
            }
        }

        /// The phases of `events`, in order.
        fn phases(events: &[Value]) -> Vec<&str> {
            events
                .iter()
                .map(|event| event["phase"].as_str().unwrap())
                .collect()
        }

        /// Checks that `events` are one whole drag on the goal closed by
        /// `closing`: a start, at least one move, then the one closing
        /// event, every one under the same gesture with the modifiers
        /// `held`. A `cancel` carries no pose; every other event does.
        fn one_drag(events: &[Value], closing: &str, held: Value) {
            let phases = phases(events);
            assert_eq!(phases.first(), Some(&"start"), "{phases:?}");
            assert!(phases.contains(&"move"), "{phases:?}");
            assert_eq!(phases.last(), Some(&closing), "{phases:?}");
            assert_eq!(
                phases
                    .iter()
                    .filter(|phase| matches!(**phase, "end" | "cancel"))
                    .count(),
                1,
                "closed once: {phases:?}"
            );
            let gesture = &events[0]["gesture"];
            assert!(gesture.is_u64(), "{:?}", events[0]);
            for event in events {
                assert_eq!(event["type"], "control");
                assert_eq!(event["key"], KEY);
                assert_eq!(&event["gesture"], gesture, "one drag, one gesture");
                assert_eq!(event["modifiers"], held);
                let cancel = event["phase"] == "cancel";
                for field in ["pose", "scale"] {
                    assert_eq!(event.get(field).is_none(), cancel, "{field} on {event}");
                }
                assert!(event.get("value").is_none());
            }
        }

        fn none_held() -> Value {
            json!({ "alt": false, "shift": false, "meta": false, "ctrl": false })
        }

        /// Modifiers set while the scene takes no requests wait as one, the
        /// last set, however many times the page sets them.
        #[test]
        fn modifiers_set_between_frames_wait_as_the_last_set() {
            let mut page = Page::new();
            for shift in [true, false, true] {
                set_modifiers(false, shift, false, false);
                cancel_drag();
            }
            let waiting: Vec<Modifiers> = REQUESTS
                .lock()
                .unwrap()
                .iter()
                .filter_map(|request| match request {
                    Request::Modifiers(held) => Some(*held),
                    _ => None,
                })
                .collect();
            assert_eq!(
                waiting,
                [Modifiers {
                    shift: true,
                    ..default()
                }]
            );
            page.app.update();
            assert!(page.app.world().resource::<Modifiers>().shift);
        }

        /// A drag let go of ends once, carrying the modifiers the page said
        /// were held, and sends nothing after it.
        #[test]
        fn a_drag_let_go_of_ends_once() {
            let mut page = Page::new();
            set_modifiers(true, false, true, false);
            page.drag();
            page.release();
            let held = json!({ "alt": true, "shift": false, "meta": true, "ctrl": false });
            one_drag(&page.sent(), "end", held);
            page.point_at(page.spot);
            assert!(page.sent().is_empty(), "nothing after the end");

            // The next drag is a gesture of its own, under what is held now.
            set_modifiers(false, false, false, false);
            page.drag();
            page.release();
            one_drag(&page.sent(), "end", none_held());
        }

        /// A drag the page cancels closes with one `cancel`, carrying no
        /// pose, and sends nothing more: not while the button stays down,
        /// nor when it comes up.
        #[test]
        fn a_drag_the_page_cancels_closes_with_cancel() {
            let mut page = Page::new();
            page.drag();
            let gesture = page.sent()[0]["gesture"].clone();
            cancel_drag();
            page.app.update();
            let closed = page.sent();
            assert_eq!(phases(&closed), ["cancel"]);
            assert_eq!(closed[0]["gesture"], gesture);
            assert_eq!(closed[0]["modifiers"], none_held());
            assert!(closed[0].get("pose").is_none());

            page.point_at(page.spot + Vec2::new(12.0, -9.0));
            page.release();
            assert!(page.sent().is_empty(), "nothing after the cancel");
        }

        /// A drag whose control the page hides while the button is down
        /// closes with one `cancel`, and its release sends nothing.
        #[test]
        fn a_drag_on_a_control_hidden_mid_drag_is_cancelled() {
            let mut page = Page::new();
            page.drag();
            control(json!({ "op": "show", "key": KEY, "visible": false }).to_string()).unwrap();
            page.point_at(page.spot + Vec2::new(12.0, -9.0));
            page.point_at(page.spot + Vec2::new(14.0, -10.0));
            page.release();
            one_drag(&page.sent(), "cancel", none_held());
        }

        /// A drag whose control the page removes while the button is down
        /// closes with one `cancel`, though there is no gizmo left to report
        /// it, and its release sends nothing.
        #[test]
        fn a_drag_on_a_control_removed_mid_drag_is_cancelled() {
            let mut page = Page::new();
            page.drag();
            control(json!({ "op": "remove", "key": KEY }).to_string()).unwrap();
            page.app.update();
            assert!(
                page.app
                    .world_mut()
                    .query::<&TransformGizmo>()
                    .iter(page.app.world())
                    .next()
                    .is_none(),
                "the control is gone"
            );
            page.point_at(page.spot + Vec2::new(12.0, -9.0));
            page.release();
            one_drag(&page.sent(), "cancel", none_held());
        }
    }
}
