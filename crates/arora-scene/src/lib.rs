//! A robot scene in Bevy: what it costs to draw, and what a second consumer of
//! the face renderer turns out to need from it.
//!
//! This is the scene itself — loading a robot, reading the `RobotData` that
//! Bevy's glTF loader drops, drawing a Vizij face into the screen the robot
//! declares, and putting a limited control on a joint. It holds no opinion
//! about how a host drives it. `main.rs` is one host, a CLI; a wasm harness is
//! another, and the measurement work needs a third that issues commands frame
//! by frame rather than flags before startup.
//!
//! **Measure with [`SceneOptions::offscreen`].** A windowed run reports the
//! display, not the renderer: wgpu cannot lift vsync on macOS, where it stays
//! on Fifo, and on a ProMotion panel the refresh rate itself moves, so the
//! same scene reads 8.33ms or 16.67ms depending on where the display has
//! settled. Both are exact refresh intervals, which is how to recognise the
//! trap. Rendering into an image leaves nothing presenting to a surface, and
//! then the frame time is the work.
//!
//! Percentiles rather than an average: a mean hides the stalls that make a
//! viewport feel bad, and the 99th is what a person notices.

pub mod aurora;
pub mod instance;
pub mod robot;
pub mod stage;
pub mod toggles;
pub mod tune;

use std::path::PathBuf;

use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::RenderLayers;
use bevy::camera::RenderTarget;
use bevy::gltf::GltfAssetLabel;
use bevy::input::mouse::MouseWheel;
use bevy::prelude::*;
use bevy::render::render_resource::{TextureFormat, TextureUsages};
use bevy::render::view::screenshot::{save_to_disk, Screenshot};

use arora_visuals::camera_input::{CameraBindings, CameraFloor, CameraInputPlugin};
use arora_visuals::ghost::{Ghost, GhostLook};
use arora_visuals::joint_gizmo::{JointGizmo, JointMotion};
use arora_visuals::outline::OutlineOverlay;
use arora_visuals::overlay::{GizmoKind, Overlay, OverlayPlugin};
use arora_visuals::transform_gizmo::{
    GizmoPart, GizmoSpace, TransformGizmo, TransformGizmoPlugin, TransformMode, TransformModes,
};
use arora_visuals::view_gizmo::{LookFrom, ViewGizmo};
use bevy::camera_controller::pan_orbit_camera::extensions::look_to::LookToTrigger;
use bevy::camera_controller::pan_orbit_camera::prelude::{OrbitConstraint, PanOrbitCamera};
use bevy::math::DVec3;

/// How the scene draws, independent of how a host chose it.
///
/// Deliberately not the CLI. A wasm host has no flags, and a measurement
/// harness sets these per run. Everything here describes *rendering*; what to
/// load is [`RobotSpec`] and the mount functions, because loading is an action
/// a host takes rather than a setting it declares.
#[derive(Resource, Clone)]
pub struct SceneOptions {
    /// Hold the camera still instead of orbiting, to separate the cost of a
    /// moving view from the cost of the scene.
    pub still: bool,
    /// Where the camera starts, as a yaw in radians about the model.
    pub angle: f32,
    /// How far above the model the camera starts, in radians.
    pub pitch: f32,
    /// Draw shadows. Off prices them separately.
    pub shadows: bool,
    /// Add a reference cube at the model's centre and clear to magenta. A
    /// capture that comes back black is ambiguous — nothing rendered, or
    /// nothing was read back — and these two separate those cases.
    pub marker: bool,
    /// Give every mesh its own material, as a renderer must when each
    /// element's colour is separately drivable. Materials are a batching key,
    /// so this prices what per-element editability costs.
    pub unique_materials: bool,
    /// Hold a joint control in its hovered state. The hover follows the
    /// pointer, so a headless capture cannot otherwise show the shaded ring.
    pub ring_active: bool,
    /// Render into an offscreen image and leave the window empty, so frame
    /// time is the work rather than the display's cadence.
    pub offscreen: bool,
    /// Stop after this many seconds and report. Zero runs until closed.
    pub seconds: f32,
    /// Save one frame here, after the warm-up.
    pub screenshot: Option<PathBuf>,
    /// Whether the host asked to present without waiting for vsync. The scene
    /// only reports this; applying it is the host's, since it is a property of
    /// the window and a wasm host has no say in it.
    pub uncapped: bool,
    /// Where the stage starts. A running scene changes it through
    /// [`stage::Stage`]; these only set the first frame.
    ///
    /// Light or dark, which both the environment and the rig follow.
    pub theme: stage::Theme,
    /// What fills the frame above the horizon.
    pub environment: stage::Environment,
    /// What the rig is fixed to.
    pub lighting: stage::Lighting,
    /// How strongly a selection is filled with its outline colour, from not
    /// at all to solid.
    pub outline_fill: f32,
    /// Put a transform gizmo on the first instance, offering these modes.
    pub gizmo: Option<TransformModes>,
    /// Whose axes that gizmo moves and turns along.
    pub gizmo_space: GizmoSpace,
    /// Draw the robots as ghosts, as Studio draws the instance showing a
    /// robot's measured state.
    pub ghost: bool,
    /// Wash ghosts toward the theme's ink rather than always toward white.
    pub ghost_wash_theme: bool,
    /// A ghost's rim strength against Studio's on the light theme, when
    /// washing toward the theme.
    pub ghost_rim: f32,
    /// Frame the view beside the test window's stand-in panel at the start.
    /// Acted on by [`toggles::TogglesPlugin`], which a host leaves out: a
    /// host frames beside its own panels with an active area of its own.
    pub panel: bool,
    /// Which reference frames show axes at the start, by the test window's
    /// toggles.
    pub frames: toggles::Frames,
    /// How they are drawn.
    pub frame_style: toggles::FrameStyle,
}

impl Default for SceneOptions {
    fn default() -> Self {
        Self {
            still: false,
            angle: 0.0,
            pitch: 0.4,
            shadows: true,
            marker: false,
            unique_materials: false,
            ring_active: false,
            offscreen: false,
            seconds: 0.0,
            screenshot: None,
            uncapped: false,
            theme: stage::Theme::default(),
            environment: stage::Environment::default(),
            lighting: stage::Lighting::default(),
            outline_fill: 0.0,
            gizmo: None,
            gizmo_space: GizmoSpace::World,
            ghost: false,
            ghost_wash_theme: true,
            ghost_rim: 2.0,
            panel: false,
            frames: toggles::Frames::Off,
            frame_style: toggles::FrameStyle::Arrows,
        }
    }
}

/// Which robot to load, and how many of it.
///
/// A resource read at startup, which is the shape this spike inherited and not
/// the shape Studio needs: an editor loads an asset after the instance is
/// already running, and swaps and removes them while it runs. Moving this to a
/// runtime command is the next change, and it is the same change the face
/// renderer needs for its own state.
#[derive(Resource, Clone)]
pub struct RobotSpec {
    /// The robot GLB, resolved by the asset server.
    pub glb: String,
    /// Spawn it this many times, in a grid. Multiplying the real asset states
    /// the headroom in the units that matter: how many robots' worth of scene
    /// a frame budget holds.
    pub copies: u32,
}

/// The scene's systems. A host adds this, inserts [`SceneOptions`] and
/// [`RobotSpec`], and runs.
pub struct ScenePlugin;

impl Plugin for ScenePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            CameraInputPlugin,
            arora_visuals::joint_gizmo::JointGizmoPlugin,
            stage::StagePlugin,
            aurora::AuroraPlugin,
            arora_visuals::outline::OutlinePlugin,
            arora_visuals::view_gizmo::ViewGizmoPlugin,
            TransformGizmoPlugin,
            OverlayPlugin,
            arora_visuals::ghost::GhostPlugin,
            arora_visuals::axes::FrameAxesPlugin,
            arora_visuals::active_area::ActiveAreaPlugin,
        ))
        .init_resource::<WashChoice>()
        // Read by the scene's gizmo systems; the test window's toggles set it.
        .init_resource::<toggles::GizmoChoice>()
        .add_systems(
            Update,
            (
                attach_gizmo,
                choose_gizmo_mode,
                ghost_robots,
                hide_controls_on_ghosts,
                wash_ghosts,
                place_readout,
            ),
        )
        .add_systems(Startup, spawn_readout)
        .init_resource::<arora_visuals::select::Selections>()
        .init_resource::<instance::Selected>()
        // Bevy's own mesh picking is not added: it casts base vertices,
        // and a pick has to meet the mesh where a morph put it. The
        // shared cast decides picks instead, from presses and releases
        // that arrive on the window entity.
        .add_plugins(arora_visuals::pick::PickPlugin)
        .add_observer(look_from)
        .init_resource::<Frames>()
        .init_resource::<Census>()
        .init_resource::<Framing>()
        .init_resource::<Capture>()
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (
                take_census,
                split_materials,
                spawn_marker,
                fit_orbit,
                count_visible,
                capture,
                measure,
            )
                .chain(),
        );
    }
}

/// Frame durations, collected after a warm-up so shader compilation and asset
/// loading are not counted as render cost.
#[derive(Resource, Default)]
pub struct Frames {
    samples: Vec<f32>,
    elapsed: f32,
    count: u64,
    reported: bool,
}

/// Proof that the frames above were paid for a robot rather than an empty
/// viewport.
///
/// Reads back the main camera's offscreen target rather than the window. A
/// capture of the window's swapchain comes back pure black whenever the window
/// is not the composited frontmost surface — which is indistinguishable from a
/// scene that genuinely drew nothing, and the frame counter is happy either
/// way. An offscreen target does not depend on the window being on screen.
#[derive(Resource, Default)]
struct Capture {
    at_frame: u64,
    done: bool,
}

/// The image the main camera renders into, when it is not drawing to the
/// window. This is what a screenshot reads.
#[derive(Resource)]
struct OffscreenView(Handle<Image>);

const CAPTURE_SECONDS: f32 = 1.0;

/// What was actually on screen while the frames above were measured. A frame
/// time means nothing without the load that produced it.
#[derive(Resource, Default)]
pub struct Census {
    meshes: usize,
    triangles: usize,
    materials: usize,
    taken: bool,
    /// The most meshes any single frame passed to the renderer, as Bevy's own
    /// frustum culling judged it. This is what says the frame times were paid
    /// for a visible robot: a camera pointed at nothing culls everything and
    /// still reports a healthy frame rate.
    visible_peak: usize,
    /// A mounted asset's own meshes, counted apart from the robot's.
    mounted_meshes: usize,
}

/// Whether an entity belongs to an asset mounted on a screen rather than to
/// the scene around it.
fn on_mounted_layer(layers: Option<&RenderLayers>) -> bool {
    layers.is_some_and(|layers| layers.intersects(&RenderLayers::layer(MOUNTED_LAYER)))
}

/// Where the model actually sits, measured rather than assumed. A glTF carries
/// no promise about its units or which way is up, and a camera placed on a
/// guess renders an empty frame that still reports a healthy frame rate — so
/// the camera is fitted to the bounds the meshes report.
#[derive(Resource, Default)]
struct Framing {
    center: Vec3,
    radius: f32,
    fitted: bool,
}

const WARMUP_SECONDS: f32 = 2.0;

/// The layer a mounted asset draws on, so the two scenes sharing this world
/// do not draw into each other's views.
const MOUNTED_LAYER: usize = 1;

/// The scene's one camera.
///
/// One for the whole world, where a face renderer gives each of its instances
/// its own: a robot scene is a single space that everything stands in, so the
/// camera belongs to the scene rather than to anything in it.
#[derive(Component)]
pub struct OrbitCamera;

/// The joint chosen for a gizmo, until its body is found and the gizmo put
/// on it; the gizmo itself is `arora_visuals`' `JointGizmo`.
#[derive(Resource)]
pub(crate) struct WantedJoint {
    joint: robot::Joint,
    /// A value asked for from outside, applied once through the gizmo's own
    /// clamp. Lets a headless run show a limit holding.
    asked: Option<f32>,
}

/// The screen the robot declared, and the texture a face is drawing into.
#[derive(Resource)]
struct ScreenMount {
    screen: robot::Screen,
    texture: Handle<Image>,
}

/// Selects the joint to put a gizmo on, or lists what there is.
pub fn mount_joint(
    app: &mut App,
    assets: &std::path::Path,
    robot_glb: &str,
    wanted: &str,
    joint_value: Option<f32>,
) {
    let joints = match std::fs::read(assets.join(robot_glb))
        .map_err(anyhow::Error::from)
        .and_then(|bytes| robot::find_joints(&bytes))
    {
        Ok(joints) => joints,
        Err(error) => {
            log::error!("could not read the robot's joints: {error:#}");
            return;
        }
    };

    if wanted == "list" {
        log::info!("{} joints:", joints.len());
        for joint in &joints {
            let limits = if joint.is_limited() {
                format!("[{:.3}, {:.3}]", joint.min, joint.max)
            } else {
                "unlimited".to_string()
            };
            log::info!(
                "  {:24} {:11} {limits:22} default {:.3}  axis {:.0?}",
                joint.name,
                joint.kind.as_str(),
                joint.default,
                joint.axis,
            );
        }
        return;
    }

    let Some(joint) = joints.into_iter().find(|j| j.name == wanted) else {
        log::error!("no joint called {wanted:?}; pass --joint list to see them");
        return;
    };
    // A fixed joint holds its child in place; offering a control for it would
    // be a handle that moves something the robot cannot move.
    if !joint.kind.is_drivable() {
        log::error!(
            "{:?} is {} — it does not move, so it takes no control",
            joint.name,
            joint.kind.as_str()
        );
        return;
    }
    log::info!(
        "gizmo on {:?} ({}), range [{:.3}, {:.3}], resting at {:.3}",
        joint.name,
        joint.kind.as_str(),
        joint.min,
        joint.max,
        joint.default
    );
    app.insert_resource(WantedJoint {
        joint,
        asked: joint_value,
    })
    .add_systems(Startup, spawn_label)
    .add_systems(Update, (attach_joint, hold_in_hand, place_label).chain());
}

/// Puts the joint gizmo on the body the joint moves, once the scene has
/// spawned.
///
/// The robot's glTF nodes carry no names, so Bevy names them `GltfNode{index}`
/// — and that index is the only thing tying `RobotData` to a spawned entity.
fn attach_joint(
    wanted: Res<WantedJoint>,
    mut commands: Commands,
    names: Query<(Entity, &Name)>,
    cameras: Query<Entity, With<OrbitCamera>>,
    gizmos: Query<(), With<JointGizmo>>,
    frames: Res<Frames>,
) {
    if !gizmos.is_empty() || frames.elapsed < 0.5 {
        return;
    }
    let joint = &wanted.joint;
    let node = format!("GltfNode{}", joint.child_node);
    let (Some((body, _)), Some(camera)) = (
        names.iter().find(|(_, name)| name.as_str() == node),
        cameras.iter().next(),
    ) else {
        return;
    };
    let motion = if joint.kind.is_angular() {
        JointMotion::Turn
    } else {
        JointMotion::Slide
    };
    let axis = Dir3::new(joint.axis).unwrap_or(Dir3::Y);
    let mut gizmo = JointGizmo::new(
        body,
        camera,
        motion,
        axis,
        joint.min,
        joint.max,
        joint.default,
        joint.color.into(),
    );
    gizmo.limited = joint.is_limited();
    if let Some(asked) = wanted.asked {
        let held = if joint.is_limited() {
            asked.clamp(joint.min, joint.max)
        } else {
            asked
        };
        if (held - asked).abs() > 1e-4 {
            log::info!("{:?} asked for {asked:.3}, held at {held:.3}", joint.name);
        }
        gizmo.value = held;
    }
    log::info!(
        "{:?} drives {node}; axis {:.2?} in its own frame",
        joint.name,
        joint.axis,
    );
    // Named in the overlay by the joint's name, which is what the label
    // finds it by.
    gizmo.key = joint.name.clone();
    commands.spawn(gizmo);
}

/// Draws the joint control as though in hand when the options ask, since a
/// headless capture has no pointer to hover it with. Only its look: the
/// pointer is left alone, so the view and picks work as ever.
fn hold_in_hand(opts: Res<SceneOptions>, mut gizmos: Query<&mut JointGizmo>) {
    for mut gizmo in &mut gizmos {
        if gizmo.show_in_hand != opts.ring_active {
            gizmo.show_in_hand = opts.ring_active;
        }
    }
}

/// The gizmo's readout: which joint, and where it stands in degrees.
///
/// Drawn as UI and placed by projecting the joint into the viewport each
/// frame — the same shape as the HTML overlay the web renderer drives from its
/// anchor callback.
#[derive(Component)]
struct JointLabel;

/// The label, and whether it has been pointed at a camera yet.
type LabelQuery<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static mut Node,
        &'static mut Visibility,
        &'static Children,
        Has<UiTargetCamera>,
    ),
    With<JointLabel>,
>;

fn spawn_label(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.09, 0.09, 0.11, 0.92)),
            JointLabel,
        ))
        .with_child((
            Text::new(""),
            TextFont {
                font_size: bevy::text::FontSize::Px(13.0),
                ..default()
            },
        ));
}

/// Places the label beside the joint's gizmo, reading where the gizmo is
/// from the overlay — the same report a web host would place a DOM label
/// from, so the two cannot disagree about where the gizmo is.
fn place_label(
    wanted: Res<WantedJoint>,
    overlay: Res<Overlay>,
    gizmos: Query<&JointGizmo>,
    cameras: Query<Entity, With<OrbitCamera>>,
    mut labels: LabelQuery,
    mut texts: Query<(&mut Text, &mut TextColor)>,
    mut commands: Commands,
) {
    let Ok((label, mut node, mut visibility, children, targeted)) = labels.single_mut() else {
        return;
    };
    let Some(camera) = cameras.iter().next() else {
        return;
    };
    // UI draws to the primary window unless told otherwise, so a label left
    // untargeted is missing from every offscreen capture while looking fine on
    // screen.
    if !targeted {
        commands.entity(label).insert(UiTargetCamera(camera));
    }
    let joint = &wanted.joint;
    let anchor = overlay
        .0
        .gizmos
        .iter()
        .find(|anchor| anchor.kind == GizmoKind::Joint && anchor.key == joint.name);
    let (Some(anchor), Some(gizmo)) = (anchor, gizmos.iter().next()) else {
        *visibility = Visibility::Hidden;
        return;
    };
    *visibility = Visibility::Inherited;
    // Clear of the ring or bar rather than over it, so the handle stays
    // legible.
    node.left = Val::Px(anchor.screen.max_x + 14.0);
    node.top = Val::Px(anchor.screen.y - 10.0);

    let colour: Color = joint.color.into();
    for child in children.iter() {
        if let Ok((mut text, mut text_colour)) = texts.get_mut(child) {
            // Bevy's bundled fallback font carries ASCII only, so a degree
            // sign renders as tofu. A real build ships a font and can use one.
            text.0 = if joint.kind.is_angular() {
                format!("{}   {:.1} deg", joint.name, gizmo.value.to_degrees())
            } else {
                // A sliding joint's value is a distance; reporting it in
                // degrees is the same category error as rotating it.
                format!("{}   {:.0} mm", joint.name, gizmo.value * 1000.0)
            };
            text_colour.0 = colour;
        }
    }
}

/// What a held transform gizmo reads, one line to each axis, beside the
/// gizmo: a host's own label, placed from the overlay as the joint's is.
#[derive(Component)]
struct Readout;

/// One line of the readout, by its place in the stack.
#[derive(Component)]
struct ReadoutLine(usize);

fn spawn_readout(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                flex_direction: FlexDirection::Column,
                padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.09, 0.09, 0.11, 0.92)),
            Visibility::Hidden,
            Readout,
        ))
        .with_children(|readout| {
            // A plane reads two axes; nothing reads more.
            for line in 0..2 {
                readout.spawn((
                    Text::new(""),
                    TextFont {
                        font_size: bevy::text::FontSize::Px(13.0),
                        ..default()
                    },
                    ReadoutLine(line),
                ));
            }
        });
}

/// Shows a held transform gizmo's readings beside it, stacked, each in its
/// axis's colour, and hides when no transform handle is held.
fn place_readout(
    overlay: Res<Overlay>,
    cameras: Query<Entity, With<OrbitCamera>>,
    mut readouts: Query<(Entity, &mut Node, &mut Visibility, Has<UiTargetCamera>), With<Readout>>,
    mut lines: Query<(&ReadoutLine, &mut Text, &mut TextColor, &mut Node), Without<Readout>>,
    mut commands: Commands,
) {
    let Ok((readout, mut node, mut visibility, targeted)) = readouts.single_mut() else {
        return;
    };
    if !targeted {
        if let Some(camera) = cameras.iter().next() {
            commands.entity(readout).insert(UiTargetCamera(camera));
        }
    }
    let frame = &overlay.0;
    let held = frame.pointer.as_ref().filter(|pointer| {
        pointer.kind == GizmoKind::Transform && pointer.held && !pointer.readings.is_empty()
    });
    let anchor = held.and_then(|pointer| {
        frame
            .gizmos
            .iter()
            .find(|anchor| anchor.kind == GizmoKind::Transform && anchor.key == pointer.key)
    });
    let (Some(pointer), Some(anchor)) = (held, anchor) else {
        visibility.set_if_neq(Visibility::Hidden);
        return;
    };
    visibility.set_if_neq(Visibility::Inherited);
    node.left = Val::Px(anchor.screen.max_x + 14.0);
    node.top = Val::Px(anchor.screen.min_y);
    for (line, mut text, mut colour, mut line_node) in &mut lines {
        let Some(reading) = pointer.readings.get(line.0) else {
            line_node.display = Display::None;
            continue;
        };
        line_node.display = Display::Flex;
        text.0 = reading_text(reading);
        colour.0 = reading
            .axis
            .map(|axis| arora_visuals::transform_gizmo::AXIS_COLOURS[axis])
            .unwrap_or(Color::WHITE);
    }
}

/// One reading as a line: its axis, its amount and its unit. Bevy's bundled
/// fallback font carries ASCII only, so degrees are spelled out.
fn reading_text(reading: &arora_visuals::overlay::Reading) -> String {
    use arora_visuals::overlay::ReadingUnit;
    let axis = match reading.axis {
        Some(0) => "X",
        Some(1) => "Y",
        Some(2) => "Z",
        _ => "",
    };
    match reading.unit {
        ReadingUnit::Metres => format!("{axis}  {:+.3} m", reading.amount),
        ReadingUnit::Degrees => format!("{axis}  {:+.1} deg", reading.amount),
        ReadingUnit::Factor => format!("{axis}  x{:.2}", reading.amount),
    }
}

/// Makes every spawned robot an entity instance the shared layer can address.
///
/// Reads what the GLB declares before the app runs, for the same reason the
/// other mounts do: a wasm host has no filesystem and an editor loads its
/// assets after startup, so driving this from the asset's own load is what
/// would let either happen.
///
/// `select` outlines those elements by their `RobotData` ids on every copy.
pub fn mount_instances(
    app: &mut App,
    assets: &std::path::Path,
    robot_glb: &str,
    select: Vec<arora_visuals::select::Selection>,
) {
    let meta = match std::fs::read(assets.join(robot_glb))
        .map_err(anyhow::Error::from)
        .and_then(|bytes| arora_visuals::meta::AssetMeta::from_glb_bytes(&bytes))
    {
        Ok(meta) => meta,
        Err(error) => {
            log::error!("could not read what {robot_glb} declares: {error:#}");
            return;
        }
    };
    log::info!(
        "{} declares {} elements and {} animatables",
        robot_glb,
        meta.elements.len(),
        meta.animatables.len()
    );
    if !select.is_empty() {
        let known: Vec<&str> = meta
            .elements
            .iter()
            .filter(|e| select.iter().any(|selection| selection.element == e.id))
            .map(|e| e.id.as_str())
            .collect();
        if known.len() < select.len() {
            log::warn!(
                "{} of {} selected ids are declared; pass --list-elements to see them",
                known.len(),
                select.len()
            );
        }
    }
    app.insert_resource(instance::RobotMeta(meta))
        .insert_resource(instance::Selected(select))
        // Registered by the mount rather than by the plugin, so the system
        // exists only where the resources it needs do.
        .add_systems(Update, instance::index_instances);
}

/// Builds the screen the robot declares and hands back the texture it shows.
///
/// A `screen` in `RobotData` is `{width, height, resolution}` with no
/// geometry — a declaration, not a mesh. **The scene owns the surface and
/// whoever draws the content owns the pixels**, and this function is that
/// seam: it sizes the texture from the screen's own metres and
/// pixels-per-metre, builds the quad, and returns the handle. Anything that
/// can render into a Bevy image can fill it, and a screen nobody fills stays
/// blank rather than failing.
///
/// Returns `None` when the robot declares no screen.
///
/// This runs before the app does, because the screen's size comes out of the
/// robot's JSON rather than out of the world — so a host that loads its
/// assets after startup needs this driven by the asset's own load, which is
/// the open piece of the dynamic-loading story rather than a detail of it.
pub fn mount_screen(
    app: &mut App,
    assets: &std::path::Path,
    robot_glb: &str,
) -> Option<Handle<Image>> {
    let screen = match std::fs::read(assets.join(robot_glb))
        .map_err(anyhow::Error::from)
        .and_then(|bytes| robot::find_screen(&bytes))
    {
        Ok(screen) => screen,
        Err(error) => {
            log::error!("no screen to build: {error:#}");
            return None;
        }
    };

    let (width, height) = screen.texture_size();
    log::info!(
        "screen: {:.3}x{:.3}m at {:.2?}, backed by a {width}x{height} texture",
        screen.width,
        screen.height,
        screen.transform.translation,
    );

    // Sized from the screen's own metres and pixels-per-metre, as Studio sizes
    // its RenderTexture. No COPY_SRC: this texture is sampled by the quad's
    // material on the GPU and never read back.
    let texture = app
        .world_mut()
        .resource_mut::<Assets<Image>>()
        .add(Image::new_target_texture(
            width,
            height,
            TextureFormat::Rgba8Unorm,
            Some(TextureFormat::Rgba8UnormSrgb),
        ));

    app.insert_resource(ScreenMount {
        screen,
        texture: texture.clone(),
    })
    // Update, not Startup: the node it attaches to does not exist until the
    // glTF scene has spawned.
    .add_systems(Update, spawn_screen);

    Some(texture)
}

/// Builds the quad the robot's screen only describes, and hangs its texture
/// on it.
///
/// The quad goes *under* the screen's own glTF node, not at the world position
/// that node resolves to. A screen is bolted to a robot that moves: parent it
/// and every joint above it carries it for free; place it in world space and
/// it stays behind the moment a neck turns.
///
/// Runs until the node appears, since the glTF scene spawns asynchronously.
fn spawn_screen(
    mount: Res<ScreenMount>,
    mut placed: Local<bool>,
    names: Query<(Entity, &Name)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut commands: Commands,
) {
    if *placed {
        return;
    }
    // Unnamed glTF nodes are named for their index, which is the only handle
    // on a node the robot's own metadata refers to.
    let wanted = format!("GltfNode{}", mount.screen.node);
    let Some((node, _)) = names.iter().find(|(_, name)| name.as_str() == wanted) else {
        return;
    };
    *placed = true;
    commands.spawn((
        Mesh3d(meshes.add(Rectangle::new(mount.screen.width, mount.screen.height))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color_texture: Some(mount.texture.clone()),
            // Whatever draws into the screen has already lit its own content,
            // so the scene's lights must not get a second say over it.
            unlit: true,
            ..default()
        })),
        Transform::IDENTITY,
        ChildOf(node),
    ));
    log::info!("screen quad attached under {wanted}");
}

pub fn setup(
    mut commands: Commands,
    opts: Res<SceneOptions>,
    spec: Res<RobotSpec>,
    assets: Res<AssetServer>,
    mut images: ResMut<Assets<Image>>,
) {
    log::info!(
        "loading {} ({})",
        spec.glb,
        if opts.uncapped { "uncapped" } else { "vsync" }
    );
    let scene = assets.load(GltfAssetLabel::Scene(0).from_asset(spec.glb.clone()));
    // A square-ish grid, spaced by roughly a robot's width. The exact spacing
    // does not matter — the camera fits whatever bounds result.
    let stride = (spec.copies as f32).sqrt().ceil().max(1.0) as u32;
    for i in 0..spec.copies.max(1) {
        let (x, z) = ((i % stride) as f32, (i / stride) as f32);
        commands.spawn((
            WorldAssetRoot(scene.clone()),
            Transform::from_xyz((x - (stride - 1) as f32 * 0.5) * 0.8, 0.0, z * 0.8),
        ));
    }

    // Placed properly once the model's bounds are known; see `take_census`.
    // Until then it stands well back from where a robot of ordinary size
    // stands, at the options' angle, so a robot arriving before its bounds
    // are known is seen whole and small rather than from inside, and the
    // camera then approaches it. Lit and finished by `stage`, which reads
    // what the host chose.
    let mut camera = commands.spawn((
        Camera3d::default(),
        standing(
            &opts,
            ORDINARY_CENTRE,
            ORDINARY_RADIUS * FRAMING_DISTANCE * STANDING_BACK,
        ),
        // Bevy's controller, under Studio's bindings: left orbits, right pans,
        // the wheel zooms. A gizmo's handle takes a press before the camera
        // does. The world is Y-up, and the view never tips over the top.
        PanOrbitCamera {
            orbit_constraint: OrbitConstraint::Fixed {
                up: DVec3::Y,
                can_pass_tdc: false,
            },
            ..default()
        },
        CameraBindings {
            floor: Some(CAMERA_FLOOR),
            ..default()
        },
        OrbitCamera,
        // Studio's corner gizmo. Studio's world is Z-up, so its letters name
        // the axes a Studio user knows rather than Bevy's.
        ViewGizmo {
            axes: ViewGizmo::Z_UP,
            ..default()
        },
    ));
    // Saving a frame implies rendering offscreen, because the window's own
    // swapchain cannot be read back here: a capture of it returns pure black
    // whenever the window is not the composited frontmost surface, which is
    // the normal case for a run started from a terminal.
    if opts.offscreen || opts.screenshot.is_some() {
        let mut target = Image::new_target_texture(
            1280,
            800,
            TextureFormat::Rgba8Unorm,
            Some(TextureFormat::Rgba8UnormSrgb),
        );
        // `new_target_texture` asks for everything needed to render *into* the
        // texture but not to copy back *out* of it, and without this the
        // readback quietly returns the zero-filled CPU side — a black image
        // indistinguishable from a scene that drew nothing.
        target.texture_descriptor.usage |= TextureUsages::COPY_SRC;
        let target = images.add(target);
        camera.insert(RenderTarget::Image(target.clone().into()));
        commands.insert_resource(OffscreenView(target));
    }
}

/// Where the camera is kept above, as Studio keeps its camera a centimetre
/// above the floor, so the view never looks up from beneath it.
pub const CAMERA_FLOOR: CameraFloor = CameraFloor {
    up: Dir3::Y,
    height: stage::FLOOR_HEIGHT + 0.01,
};

/// How far out a framed camera stands, in the model's bounding radii: far
/// enough that its bounding sphere fits the view with a little air.
const FRAMING_DISTANCE: f32 = 2.6;

/// What the camera stands back from before a model's bounds are known: a
/// robot of ordinary size standing on the floor, about a metre and a half
/// tall.
const ORDINARY_CENTRE: Vec3 = Vec3::new(0.0, 0.75, 0.0);
const ORDINARY_RADIUS: f32 = 0.85;

/// How much farther back than framing the camera starts: far enough that a
/// robot well beyond ordinary size is still seen whole before it is framed.
const STANDING_BACK: f32 = 2.0;

/// How long the camera takes to approach the framed view: Studio's, as its
/// scene's bounds fit takes.
const GLIDE_SECONDS: f32 = 1.0;

/// Where a camera stands to frame `centre` from `distance` away, at the
/// options' angle round it and height above it, looking at it.
fn standing(opts: &SceneOptions, centre: Vec3, distance: f32) -> Transform {
    let toward = Vec3::new(
        opts.angle.sin() * opts.pitch.cos(),
        opts.pitch.sin(),
        opts.angle.cos() * opts.pitch.cos(),
    );
    Transform::from_translation(centre + toward * distance).looking_at(centre, Vec3::Y)
}

/// A camera easing from where it stood to the framed view.
#[derive(Component)]
struct Glide {
    from: Transform,
    to: Transform,
    elapsed: f32,
}

impl Glide {
    /// Where the camera stands now: quick to leave and slow to arrive, so
    /// the view settles onto the framing rather than stopping on it — on
    /// Studio's curve for its bounds fit.
    fn at(&self) -> Transform {
        let t = (self.elapsed / GLIDE_SECONDS).clamp(0.0, 1.0);
        // The curve stops just short of one; the glide's last frame is the
        // framing itself.
        if t >= 1.0 {
            return self.to;
        }
        let eased = 1.0 - (-5.0 * t).exp() + 0.007 * t;
        Transform {
            translation: self.from.translation.lerp(self.to.translation, eased),
            rotation: self.from.rotation.slerp(self.to.rotation, eased),
            scale: self.to.scale,
        }
    }
}

/// Frames the model once its bounds are known, and turns the view about it
/// when the options ask for a moving camera.
///
/// The controller works from wherever the camera stands, so framing is
/// placing the camera: at the options' angle and height about the model's
/// centre, far enough out that its bounding sphere fits the view with a little
/// air, looking at the centre. A person watching sees it approach from where
/// it stood; a capture or a measurement has it placed at once. The controller is told how deep that centre is,
/// so a gesture begun over empty space anchors there rather than somewhere
/// arbitrary.
#[allow(clippy::type_complexity)]
fn fit_orbit(
    opts: Res<SceneOptions>,
    time: Res<Time>,
    framing: Res<Framing>,
    pointer: Option<Res<arora_visuals::transform_gizmo::GizmoPointer>>,
    mut scrolls: MessageReader<MouseWheel>,
    mut cameras: Query<
        (
            Entity,
            &mut Transform,
            &mut PanOrbitCamera,
            &mut Projection,
            &mut CameraBindings,
            Option<&mut Glide>,
        ),
        With<OrbitCamera>,
    >,
    mut commands: Commands,
) {
    // A scroll this frame: the zoom moves the camera itself, outside the
    // controller's motion, so a glide must give way to it as to a gesture.
    let scrolled = scrolls.read().count() > 0;
    // A gizmo drag casts its ray from the camera, so a camera that turns
    // under it would move the handle with the pointer standing still.
    let dragging = pointer.is_some_and(|pointer| pointer.dragging.is_some());
    if !framing.fitted {
        return;
    }
    let distance = framing.radius * FRAMING_DISTANCE;
    for (camera, mut transform, mut controller, mut projection, mut bindings, glide) in &mut cameras
    {
        if framing.is_changed() {
            let framed = standing(&opts, framing.center, distance);
            // A capture or a measurement wants the framed view at once; a
            // person watching gets there by a glide, not a jump.
            if opts.offscreen || opts.screenshot.is_some() {
                *transform = framed;
            } else {
                commands.entity(camera).insert(Glide {
                    from: *transform,
                    to: framed,
                    elapsed: 0.0,
                });
            }
            controller.last_anchor_depth = -f64::from(distance);
            // Near enough to look a part over, never into it; far enough to
            // see the robot whole, never so far it shrinks to nothing.
            bindings.min_distance = framing.radius * 0.4;
            bindings.max_distance = framing.radius * 20.0;

            // The default near/far pair assumes a metre-scale world; a model in
            // millimetres would sit entirely behind the far plane. The far
            // plane also has to reach the stage's ground most of the way to the
            // horizon, or a band of whatever lies beyond shows beneath it.
            *projection = Projection::Perspective(PerspectiveProjection {
                near: framing.radius * 0.01,
                far: (framing.radius * 40.0).max(stage::GROUND_REACH),
                ..default()
            });
        }

        // A glide gives way to a gesture at once, leaving the camera where
        // the person took it; otherwise it eases on to the framed view.
        if let Some(mut glide) = glide {
            if controller.current_motion.is_moving() || scrolled {
                commands.entity(camera).remove::<Glide>();
            } else {
                glide.elapsed += time.delta_secs();
                *transform = glide.at();
                if glide.elapsed >= GLIDE_SECONDS {
                    commands.entity(camera).remove::<Glide>();
                }
                continue;
            }
        }

        // A turning camera is what the frame-time measurement wants; a still
        // one is what inspecting a joint wants. It turns only while nobody is
        // moving it, so a gesture is never fought.
        if !opts.still && !controller.current_motion.is_moving() && !dragging {
            transform.rotate_around(
                framing.center,
                Quat::from_rotation_y(time.delta_secs() * 0.35),
            );
        }
    }
}

/// Ghosts each robot as it appears, when the options ask for it.
fn ghost_robots(
    opts: Res<SceneOptions>,
    mut commands: Commands,
    instances: Query<Entity, Added<arora_visuals::EntityInstance>>,
) {
    if !opts.ghost {
        return;
    }
    for instance in &instances {
        commands.entity(instance).insert(Ghost);
    }
}

/// How ghosts show: washed toward the theme's ink rather than white, and how
/// strong their rim is on the light theme. The toggles change it, and the
/// options set where it starts.
#[derive(Resource, Default, Clone, Copy, PartialEq)]
pub struct WashChoice {
    pub theme: bool,
    /// The rim's strength against Studio's on the light theme, where a dark
    /// wash on a light ground holds its outline less than white on dark.
    pub light_rim: f32,
    /// Whether the options have been read into it yet.
    started: bool,
}

/// Sets how ghosts show, from the theme and the choice.
fn wash_ghosts(
    opts: Res<SceneOptions>,
    stage: Option<Res<stage::Stage>>,
    mut choice: ResMut<WashChoice>,
    mut look: ResMut<GhostLook>,
) {
    if !choice.started {
        choice.theme = opts.ghost_wash_theme;
        choice.light_rim = opts.ghost_rim;
        choice.started = true;
    }
    let light = stage.is_some_and(|stage| stage.theme == stage::Theme::Light);
    let wanted = if choice.theme && light {
        GhostLook {
            wash: stage::LIGHT_INK,
            rim: choice.light_rim,
        }
    } else {
        GhostLook::default()
    };
    if *look != wanted {
        *look = wanted;
    }
}

/// Hides a joint control while the body it moves is ghosted, as Studio does:
/// a ghost shows where the robot is, not something to take hold of.
/// Every gizmo, of either kind, with its visibility.
type Controls<'w, 's> = Query<
    'w,
    's,
    (
        Option<&'static JointGizmo>,
        Option<&'static TransformGizmo>,
        &'static mut Visibility,
    ),
    Or<(With<JointGizmo>, With<TransformGizmo>)>,
>;

fn hide_controls_on_ghosts(
    mut gizmos: Controls,
    ghosts: Query<(), With<Ghost>>,
    parents: Query<&ChildOf>,
) {
    for (joint, transform, mut visibility) in &mut gizmos {
        let Some(target) = joint
            .map(|gizmo| gizmo.target)
            .or(transform.map(|gizmo| gizmo.target))
        else {
            continue;
        };
        let ghosted = std::iter::once(target)
            .chain(parents.iter_ancestors(target))
            .any(|entity| ghosts.contains(entity));
        let wanted = if ghosted {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        };
        if *visibility != wanted {
            *visibility = wanted;
        }
    }
}

/// Puts a transform gizmo on the first instance once there is one, when the
/// options ask for it.
fn attach_gizmo(
    opts: Res<SceneOptions>,
    mut commands: Commands,
    instances: Query<(Entity, &arora_visuals::EntityInstance)>,
    cameras: Query<Entity, With<OrbitCamera>>,
    gizmos: Query<(), With<TransformGizmo>>,
) {
    let Some(modes) = opts.gizmo else {
        return;
    };
    if !gizmos.is_empty() {
        return;
    }
    let (Some((target, instance)), Some(camera)) = (instances.iter().next(), cameras.iter().next())
    else {
        return;
    };
    commands.spawn(TransformGizmo {
        // Named in the overlay by the instance it moves.
        key: instance.id.clone(),
        modes,
        space: opts.gizmo_space,
        // Labelled as the view gizmo is, for Studio's Z-up world, so the two
        // agree on which way each axis points.
        axes: TransformGizmo::Z_UP,
        ..TransformGizmo::new(target, camera)
    });
}

/// While M, R or S is held, the gizmo shows only its moving, turning or
/// scaling handles; otherwise it shows what the toggles chose.
fn choose_gizmo_mode(
    keys: Res<ButtonInput<KeyCode>>,
    picked: Res<toggles::GizmoChoice>,
    mut gizmos: Query<&mut TransformGizmo>,
) {
    let only = if keys.pressed(KeyCode::KeyM) {
        Some(TransformMode::Translate)
    } else if keys.pressed(KeyCode::KeyR) {
        Some(TransformMode::Rotate)
    } else if keys.pressed(KeyCode::KeyS) {
        Some(TransformMode::Scale)
    } else {
        picked.0
    };
    for mut gizmo in &mut gizmos {
        if gizmo.only != only {
            gizmo.only = only;
        }
    }
}

/// Turns the camera toward a view the view gizmo asked for, with the
/// controller's own `look_to`: it swings about its anchor until it faces the
/// way the request names, from the side the request names, keeping the
/// world's up where it can.
fn look_from(
    look: On<LookFrom>,
    cameras: Query<(&GlobalTransform, &PanOrbitCamera), With<OrbitCamera>>,
    mut turns: MessageWriter<LookToTrigger>,
) {
    let Ok((at, controller)) = cameras.get(look.camera) else {
        return;
    };
    // Looking from a side is facing back across from it.
    let facing = -look.from.as_vec3().as_dvec3();
    turns.write(LookToTrigger::auto_snap_up_direction(
        facing,
        look.camera,
        &at.rotation().as_dquat(),
        controller,
    ));
}

/// The meshes a census counts: everything but the stage.
type CensusMeshes<'w, 's> = Query<
    'w,
    's,
    (
        &'static Mesh3d,
        &'static GlobalTransform,
        Option<&'static Aabb>,
        Option<&'static RenderLayers>,
    ),
    (
        Without<stage::StageMesh>,
        Without<OutlineOverlay>,
        Without<GizmoPart>,
    ),
>;

/// The meshes whose visibility is counted: everything but the stage.
type VisibleMeshes<'w, 's> = Query<
    'w,
    's,
    (&'static ViewVisibility, Option<&'static RenderLayers>),
    (
        With<Mesh3d>,
        Without<stage::StageMesh>,
        Without<OutlineOverlay>,
        Without<GizmoPart>,
    ),
>;

/// The robot's meshes with a material, its own or a ghost's in its place.
type MaterialMeshes<'w, 's> = Query<
    'w,
    's,
    (),
    (
        Or<(
            With<MeshMaterial3d<StandardMaterial>>,
            With<MeshMaterial3d<arora_visuals::ghost::GhostMaterial>>,
        )>,
        Without<stage::StageMesh>,
        Without<GizmoPart>,
    ),
>;

/// Counts what the glTF actually brought in, and measures where it sits.
///
/// Waits out the first half-second: transform propagation and bounds
/// calculation both run after `Update`, so a census taken on the frame the
/// meshes appear would measure them at the origin with no bounds at all.
fn take_census(
    frames: Res<Frames>,
    mut census: ResMut<Census>,
    mut framing: ResMut<Framing>,
    meshes: CensusMeshes,
    materials: MaterialMeshes,
    assets: Res<Assets<Mesh>>,
) {
    if census.taken || frames.elapsed < 0.5 {
        return;
    }
    let mut count = 0;
    let mut triangles = 0;
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for (handle, global, aabb, layers) in &meshes {
        // The face lives in this world too, off to one side and metres wide.
        // Counting it would price the wrong scene, and fitting the camera to
        // it would push the robot into the distance.
        if on_mounted_layer(layers) {
            continue;
        }
        count += 1;
        if let Some(mesh) = assets.get(&handle.0) {
            triangles += match mesh.indices() {
                Some(indices) => indices.len() / 3,
                None => mesh.count_vertices() / 3,
            };
        }
        let Some(aabb) = aabb else { continue };
        // Every corner, because a rotated box's extents are not its transform's.
        for i in 0..8 {
            let corner = Vec3::new(
                if i & 1 == 0 { -1.0 } else { 1.0 },
                if i & 2 == 0 { -1.0 } else { 1.0 },
                if i & 4 == 0 { -1.0 } else { 1.0 },
            );
            let local = Vec3::from(aabb.center) + Vec3::from(aabb.half_extents) * corner;
            let world = global.transform_point(local);
            min = min.min(world);
            max = max.max(world);
        }
    }
    if count == 0 || !min.is_finite() || !max.is_finite() {
        return;
    }
    // Field by field, so the running visibility peak is not reset.
    census.meshes = count;
    census.triangles = triangles;
    census.materials = materials.iter().count();
    census.taken = true;
    census.mounted_meshes = meshes
        .iter()
        .filter(|(_, _, _, layers)| on_mounted_layer(*layers))
        .count();
    *framing = Framing {
        center: (min + max) * 0.5,
        radius: ((max - min).length() * 0.5).max(f32::EPSILON),
        fitted: true,
    };
    log::info!(
        "scene: {} meshes, {} triangles, {} materials",
        census.meshes,
        census.triangles,
        census.materials
    );
    log::info!(
        "bounds: min {:.2?} max {:.2?} — {:.2} across, centred at {:.2?}",
        min,
        max,
        (max - min).length(),
        framing.center,
    );
}

/// Gives every mesh its own copy of its material.
///
/// What a renderer has to do when each element's colour, opacity or emissive
/// is separately drivable — the face crate does exactly this per element, for
/// exactly this reason. Done once, when the scene has finished spawning.
fn split_materials(
    opts: Res<SceneOptions>,
    frames: Res<Frames>,
    mut done: Local<bool>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    meshes: Query<(Entity, &MeshMaterial3d<StandardMaterial>), With<Mesh3d>>,
    mut commands: Commands,
) {
    if *done || !opts.unique_materials || frames.elapsed < 0.5 {
        return;
    }
    let mut split = 0;
    for (entity, material) in &meshes {
        let Some(own) = materials.get(&material.0).cloned() else {
            continue;
        };
        commands
            .entity(entity)
            .insert(MeshMaterial3d(materials.add(own)));
        split += 1;
    }
    if split > 0 {
        *done = true;
        log::info!("gave {split} meshes their own material");
    }
}

/// Tracks how many meshes survived culling, which is the engine's own answer
/// to "is the camera looking at the robot".
fn count_visible(mut census: ResMut<Census>, meshes: VisibleMeshes) {
    let visible = meshes
        .iter()
        .filter(|(visible, layers)| visible.get() && !on_mounted_layer(*layers))
        .count();
    census.visible_peak = census.visible_peak.max(visible);
}

/// An unmissable cube at the model's centre, sized to it. If this renders and
/// the robot does not, the problem is the robot rather than the view.
fn spawn_marker(
    opts: Res<SceneOptions>,
    framing: Res<Framing>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut commands: Commands,
    mut spawned: Local<bool>,
) {
    if !opts.marker || *spawned || !framing.fitted {
        return;
    }
    *spawned = true;
    let size = framing.radius * 0.5;
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(size, size, size))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.1, 1.0, 0.2),
            unlit: true,
            ..default()
        })),
        Transform::from_translation(framing.center),
    ));
    log::info!("marker cube: {:.2}m at {:.2?}", size, framing.center);
}

/// Renders one frame into an offscreen target and reads it back, in three
/// steps: point a second camera at the target, give it two frames to draw, then
/// save it and take the camera away again.
fn capture(
    opts: Res<SceneOptions>,
    frames: Res<Frames>,
    framing: Res<Framing>,
    view: Option<Res<OffscreenView>>,
    mut capture: ResMut<Capture>,
    mut commands: Commands,
) {
    let (Some(path), Some(view)) = (opts.screenshot.clone(), view) else {
        return;
    };
    if capture.done || !framing.fitted || frames.elapsed < CAPTURE_SECONDS {
        return;
    }
    // A couple of frames after the camera was placed, so the target holds the
    // fitted view rather than the one before it.
    if capture.at_frame == 0 {
        capture.at_frame = frames.count;
        return;
    }
    if frames.count < capture.at_frame + 3 {
        return;
    }
    capture.done = true;
    log::info!("saving a frame to {}", path.display());
    commands
        .spawn(Screenshot::image(view.0.clone()))
        .observe(save_to_disk(path));
}

fn measure(
    opts: Res<SceneOptions>,
    time: Res<Time>,
    mut frames: ResMut<Frames>,
    census: Res<Census>,
    mut exit: MessageWriter<AppExit>,
) {
    let dt = time.delta_secs();
    frames.elapsed += dt;
    frames.count += 1;
    if frames.elapsed > WARMUP_SECONDS {
        frames.samples.push(dt * 1000.0);
    }
    if opts.seconds > 0.0 && frames.elapsed >= opts.seconds && !frames.reported {
        frames.reported = true;
        report(&opts, &frames, &census);
        exit.write(AppExit::Success);
    }
}

pub fn report(opts: &SceneOptions, frames: &Frames, census: &Census) {
    if frames.samples.is_empty() {
        log::warn!("no frames measured");
        return;
    }
    let mut sorted = frames.samples.clone();
    sorted.sort_by(f32::total_cmp);
    let at = |q: f32| sorted[((sorted.len() - 1) as f32 * q) as usize];
    let mean: f32 = sorted.iter().sum::<f32>() / sorted.len() as f32;
    log::info!(
        "{} frames over {:.1}s — {} camera, shadows {}, {}",
        sorted.len(),
        frames.elapsed - WARMUP_SECONDS,
        if opts.still { "still" } else { "orbiting" },
        if !opts.shadows { "off" } else { "on" },
        if opts.uncapped { "uncapped" } else { "vsync" },
    );
    log::info!(
        "frame ms  p50 {:.2}  p95 {:.2}  p99 {:.2}  max {:.2}  mean {:.2}  ({:.0} fps at p50)",
        at(0.50),
        at(0.95),
        at(0.99),
        sorted[sorted.len() - 1],
        mean,
        1000.0 / at(0.50),
    );
    if census.taken {
        log::info!(
            "drawing {}/{} meshes visible after culling / {} triangles / {} materials{}",
            census.visible_peak,
            census.meshes,
            census.triangles,
            census.materials,
            match census.mounted_meshes {
                0 => String::new(),
                n => format!(" / plus {n} face meshes on their own layer"),
            }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the view gizmo's request asks the controller to face, looking
    /// from `from`.
    fn facing_from(from: Dir3) -> DVec3 {
        let mut app = App::new();
        app.add_message::<LookToTrigger>().add_observer(look_from);
        let camera = app
            .world_mut()
            .spawn((
                GlobalTransform::default(),
                PanOrbitCamera {
                    orbit_constraint: OrbitConstraint::Fixed {
                        up: DVec3::Y,
                        can_pass_tdc: false,
                    },
                    ..default()
                },
                OrbitCamera,
            ))
            .id();
        app.world_mut().trigger(LookFrom { camera, from });
        let turns = app.world().resource::<Messages<LookToTrigger>>();
        let mut reader = turns.get_cursor();
        let turn = reader.read(turns).next().expect("a turn was asked for");
        assert_eq!(turn.camera, camera);
        turn.target_facing_direction
    }

    /// Looking from a side faces back across from it, for every side, up and
    /// down included.
    #[test]
    fn looking_from_a_side_faces_back_across_it() {
        for from in [
            Dir3::X,
            Dir3::NEG_X,
            Dir3::Z,
            Dir3::NEG_Z,
            Dir3::Y,
            Dir3::NEG_Y,
        ] {
            let facing = facing_from(from);
            assert!(
                facing.abs_diff_eq(-from.as_vec3().as_dvec3(), 1e-9),
                "{from:?} faces {facing:?}"
            );
        }
    }
}
