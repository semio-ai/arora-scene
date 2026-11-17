//! The browser host for the scene: the wasm module `@semio/scene` loads.
//!
//! A page calls [`mount`] once with a canvas and the robot's GLB bytes, then
//! drives the scene with [`set_value`] and hears from it through the handler
//! it gives [`set_event_handler`].
//!
//! **One scene per page, for the life of the page.** Bevy's windowing runs on
//! winit, and winit allows one event loop per page and never returns from it,
//! so a second `mount` is refused rather than attempted. A host that shows and
//! hides the scene keeps its canvas mounted and hides that.
//!
//! The GLB is handed over as bytes and served to Bevy's asset server from
//! memory, so the page decides where the robot comes from (a URL, an upload, a
//! database) and the scene never fetches anything itself.

use std::cell::RefCell;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use arora_scene::controls::{ControlCommand, ControlCommands, ControlEvent, ControlEvents};
use arora_scene::instance::{RobotMeta, Selected};
use arora_scene::redraw::RedrawOnChangePlugin;
use arora_scene::stage::{Environment, Theme};
use arora_scene::values::{ApplyValues, ValueWrite, ValueWrites};
use arora_scene::{RobotSpec, SceneOptions, ScenePlugin};
use arora_visuals::active_area::ActiveArea;
use arora_visuals::meta::FeatureKind;
use arora_visuals::overlay::Overlay;
use arora_visuals::pick::Picks;
use arora_visuals::select::Selection;
use arora_visuals::transform_gizmo::TransformModes;
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

/// The asset source the robot is served from, and its path within it.
const SOURCE: &str = "memory";
const ROBOT: &str = "robot.glb";

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
    /// Element ids to select from the start.
    select: Vec<String>,
    /// Show the transform gizmo.
    gizmo: bool,
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

/// Starts the scene in the canvas whose CSS selector is `canvas` (`#scene`),
/// drawing the robot in `glb`, with `options` as JSON (see the package's
/// `SceneOptions` type). Returns once the scene is running; the robot appears
/// over the next frames.
#[wasm_bindgen]
pub fn mount(canvas: String, glb: Vec<u8>, options: String) -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    if MOUNTED.load(Ordering::SeqCst) {
        return Err(error("the scene is already mounted; a page holds one"));
    }
    mark("start");
    let opts: Options = serde_json::from_str(&options).map_err(error)?;
    let meta = arora_visuals::meta::AssetMeta::from_glb_bytes(&glb)
        .map_err(|e| error(format!("{e:#}")))?;
    // A robot whose joints cannot be read still draws; its joints just stay
    // where the GLB put them.
    let joints = arora_scene::robot::find_joints(&glb).unwrap_or_else(|e| {
        log::warn!("could not read the robot's joints: {e:#}");
        Vec::new()
    });
    mark("meta-parsed");
    // Claimed only once the inputs are known good, so a mount refused for bad
    // options or a bad robot leaves the page free to mount again.
    if MOUNTED.swap(true, Ordering::SeqCst) {
        return Err(error("the scene is already mounted; a page holds one"));
    }

    let robot = Dir::default();
    robot.insert_asset(Path::new(ROBOT), glb);

    let mut resolution = WindowResolution::default();
    if let Some(sf) = opts.scale_factor {
        resolution.set_scale_factor_override(Some(sf));
    }

    #[cfg(feature = "bench")]
    let (copies, still, cpu_preprocess) = (
        opts.bench.copies.max(1),
        opts.bench.still,
        opts.bench.cpu_preprocess,
    );
    #[cfg(not(feature = "bench"))]
    let (copies, still, cpu_preprocess) = (1, false, false);

    let mut app = App::new();
    // Sources are built when the asset plugin is added, so this comes first.
    app.register_asset_source(
        SOURCE,
        AssetSourceBuilder::new(move || {
            Box::new(MemoryAssetReader {
                root: robot.clone(),
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
        gizmo: opts.gizmo.then_some(TransformModes::ALL),
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
    .insert_resource(RobotSpec {
        glb: format!("{SOURCE}://{ROBOT}"),
        copies,
    })
    // What `arora_scene::mount_instances` does from a file, done from bytes.
    .insert_resource(RobotMeta(meta))
    .insert_resource(arora_scene::joints::RobotJoints(joints))
    .insert_resource(Selected(opts.select.iter().map(Selection::of).collect()))
    .add_systems(Update, arora_scene::instance::index_instances)
    .add_systems(Startup, keep_waker)
    .add_systems(Update, take_pushed.before(ApplyValues))
    // Before Update, where the scene carries control commands out.
    .add_systems(PreUpdate, take_controls)
    .add_systems(Last, emit_events);

    #[cfg(feature = "bench")]
    let continuous = bench::install(&mut app, &opts.bench);
    #[cfg(not(feature = "bench"))]
    let continuous = false;
    if !continuous {
        app.add_plugins(RedrawOnChangePlugin);
    }
    if let Some([x, y, w, h]) = opts.active_area {
        app.insert_resource(Area(Rect::new(x, y, x + w, y + h)))
            .add_systems(Update, place_area);
    }
    mark("app-built");
    app.run();
    Ok(())
}

#[derive(Resource)]
struct Area(Rect);

fn place_area(
    area: Res<Area>,
    cameras: Query<Entity, (With<arora_scene::OrbitCamera>, Without<ActiveArea>)>,
    mut commands: Commands,
) {
    for camera in &cameras {
        commands.entity(camera).insert(ActiveArea(area.0));
    }
}

thread_local! {
    /// Wakes the event loop from outside it. The page calls in between
    /// frames, when a loop that draws only on change may be waiting
    /// indefinitely.
    static WAKE: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
    /// Where the scene's events go: the page's handler.
    static HANDLER: RefCell<Option<js_sys::Function>> = const { RefCell::new(None) };
}

fn wake() {
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

/// Writes waiting for the next frame, each with when it was written.
static PUSHED: Mutex<Vec<(ValueWrite, f64)>> = Mutex::new(Vec::new());

/// Hands the page's writes to the scene's one place for applied values.
fn take_pushed(mut writes: ResMut<ValueWrites>) {
    let pushed = std::mem::take(&mut *PUSHED.lock().unwrap());
    for (write, _at) in pushed {
        writes.0.push(write);
        #[cfg(feature = "bench")]
        bench::applying(_at);
    }
}

/// Writes one value, one number or three, to `target`: an animatable's id as
/// the robot's `RobotData` declares it, which carries its own feature (a
/// joint's value among them), or an element's id with the `feature` to change
/// as `RobotData` names it (`rotation`, `translation`, `color`, ...). An empty
/// `feature` writes an animatable. Takes effect in the next frame, which the
/// call wakes the scene to draw.
#[wasm_bindgen]
pub fn set_value(target: String, feature: String, values: Vec<f32>) -> Result<(), JsValue> {
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
    PUSHED.lock().unwrap().push((
        ValueWrite {
            instance: None,
            target,
            feature,
            value,
        },
        now(),
    ));
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
}

/// Controls waiting for the next frame.
static CONTROLS: Mutex<Vec<ControlCommand>> = Mutex::new(Vec::new());

/// Adds, removes, shows, hides or moves a control, by a command as JSON (see
/// the package's `ControlCommand` type). Takes effect in the next frame, or,
/// before the robot has spawned, in the frame it arrives in.
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

/// Sends the frame's overlay when it changed, and every pick since the last
/// frame. Picks are drained either way, so a page that sets its handler late
/// does not get a backlog.
fn emit_events(
    overlay: Res<Overlay>,
    mut picks: ResMut<Picks>,
    mut controls: ResMut<ControlEvents>,
) {
    for event in std::mem::take(&mut controls.0) {
        send(&SceneEvent::Control(&event));
    }
    let picks = std::mem::take(&mut picks.0);
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
