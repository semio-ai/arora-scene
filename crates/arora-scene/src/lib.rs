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

use std::f32::consts::{PI, TAU};
use std::path::PathBuf;

use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::RenderLayers;
use bevy::camera::RenderTarget;
use bevy::gltf::GltfAssetLabel;
use bevy::prelude::*;
use bevy::render::render_resource::{TextureFormat, TextureUsages};
use bevy::render::view::screenshot::{save_to_disk, Screenshot};

use arora_visuals::joint_gizmo::{JointGizmo, JointMotion};
use arora_visuals::outline::OutlineOverlay;
use arora_visuals::transform_gizmo::{
    GizmoHandle, GizmoPart, GizmoPointer, GizmoSize, GizmoSpace, TransformGizmo,
    TransformGizmoPlugin, TransformGizmoSystems, TransformMode, TransformModes,
};
use arora_visuals::view_gizmo::{LookFrom, ViewGizmo};
use bevy_panorbit_camera::{PanOrbitCamera, PanOrbitCameraPlugin, PanOrbitCameraSystemSet};

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
            PanOrbitCameraPlugin,
            arora_visuals::joint_gizmo::JointGizmoPlugin,
            stage::StagePlugin,
            aurora::AuroraPlugin,
            arora_visuals::outline::OutlinePlugin,
            arora_visuals::view_gizmo::ViewGizmoPlugin,
            TransformGizmoPlugin,
            toggles::TogglesPlugin,
        ))
        .init_resource::<OrbitHold>()
        .add_systems(Update, (attach_gizmo, choose_gizmo_mode))
        .add_systems(
            Update,
            hold_orbit
                .after(TransformGizmoSystems)
                .before(PanOrbitCameraSystemSet),
        )
        .init_resource::<arora_visuals::Picks>()
        .init_resource::<arora_visuals::select::Selections>()
        .init_resource::<instance::Selected>()
        // Bevy's own mesh picking is not added: it casts base vertices,
        // and a pick has to meet the mesh where a morph put it. The
        // shared cast is reached through this observer instead, and the
        // press that drives it arrives on the window entity.
        .add_observer(arora_visuals::pick::on_press)
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
struct WantedJoint {
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
    .add_systems(
        Update,
        (
            attach_joint,
            hold_in_hand.after(arora_visuals::transform_gizmo::TransformGizmoSystems),
            place_label,
        )
            .chain(),
    );
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
    if let Some(asked) = wanted.asked {
        let held = if gizmo.is_limited() {
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
    commands.spawn(gizmo);
}

/// Holds the joint control in hand when the options ask, since a headless
/// capture has no pointer to hover it with.
fn hold_in_hand(
    opts: Res<SceneOptions>,
    gizmos: Query<Entity, With<JointGizmo>>,
    mut pointer: ResMut<GizmoPointer>,
) {
    if !opts.ring_active || pointer.hovered.is_some() || pointer.dragging.is_some() {
        return;
    }
    if let Some(gizmo) = gizmos.iter().next() {
        pointer.hovered = Some((gizmo, GizmoHandle::Joint));
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

fn place_label(
    wanted: Res<WantedJoint>,
    gizmos: Query<&JointGizmo>,
    bodies: Query<&GlobalTransform>,
    cameras: Query<(Entity, &Camera, &GlobalTransform), With<OrbitCamera>>,
    mut labels: LabelQuery,
    mut texts: Query<(&mut Text, &mut TextColor)>,
    mut commands: Commands,
) {
    let Ok((label, mut node, mut visibility, children, targeted)) = labels.single_mut() else {
        return;
    };
    let Some((camera_entity, camera, camera_at)) = cameras.iter().next() else {
        return;
    };
    // UI draws to the primary window unless told otherwise, so a label left
    // untargeted is missing from every offscreen capture while looking fine on
    // screen.
    if !targeted {
        commands.entity(label).insert(UiTargetCamera(camera_entity));
    }
    let Some(gizmo) = gizmos.iter().next() else {
        *visibility = Visibility::Hidden;
        return;
    };
    let Ok(anchor) = bodies.get(gizmo.target).map(|body| body.translation()) else {
        return;
    };
    let Ok(screen) = camera.world_to_viewport(camera_at, anchor) else {
        *visibility = Visibility::Hidden;
        return;
    };
    *visibility = Visibility::Inherited;
    // Clear of the ring rather than over it, so the handle stays legible. The
    // ring's own screen size is what decides how far clear that is: its
    // radius is the gizmo's unit times the transform gizmo's ring's.
    let radius = match gizmo.size {
        GizmoSize::World(metres) => camera
            .world_to_viewport(
                camera_at,
                anchor + camera_at.right() * metres * arora_visuals::transform_gizmo::RING_RADIUS,
            )
            .map(|edge| (edge.x - screen.x).abs())
            .unwrap_or(40.0),
        GizmoSize::Pixels(pixels) => pixels * arora_visuals::transform_gizmo::RING_RADIUS,
    };
    node.left = Val::Px(screen.x + radius + 14.0);
    node.top = Val::Px(screen.y - 10.0);

    let joint = &wanted.joint;
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
    // Lit and finished by `stage`, which reads what the host chose.
    let mut camera = commands.spawn((
        Camera3d::default(),
        // three.js OrbitControls' bindings, which is what the scene's users
        // already have in their hands: left orbits, right pans. The joint
        // gizmo shares the left button and takes priority when the press
        // lands on it.
        PanOrbitCamera {
            button_orbit: MouseButton::Left,
            button_pan: MouseButton::Right,
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

/// Points the camera controller at the model, once its bounds are known.
///
/// The controls themselves are `bevy_panorbit_camera`'s. A hand-rolled orbit
/// gets drag direction wrong on trackpads and has no inertia, focus handling
/// or zoom limits, and none of that is what this spike is measuring.
///
/// Orbit is on the right button and pan on the middle one, because the left
/// belongs to the joint gizmo — a camera control and a joint control competing
/// for one drag makes both feel broken.
fn fit_orbit(
    opts: Res<SceneOptions>,
    time: Res<Time>,
    framing: Res<Framing>,
    mut cameras: Query<(&mut PanOrbitCamera, &mut Projection), With<OrbitCamera>>,
) {
    if !framing.fitted {
        return;
    }
    // Far enough out that the bounding sphere fits the vertical field of view,
    // with a little air around it.
    let distance = framing.radius * 2.6;
    for (mut orbit, mut projection) in &mut cameras {
        if framing.is_changed() {
            orbit.focus = framing.center;
            orbit.target_focus = framing.center;
            orbit.radius = Some(distance);
            orbit.target_radius = distance;
            orbit.yaw = Some(opts.angle);
            orbit.target_yaw = opts.angle;
            orbit.pitch = Some(opts.pitch);
            orbit.target_pitch = opts.pitch;
            orbit.zoom_lower_limit = framing.radius * 0.4;
            orbit.zoom_upper_limit = Some(framing.radius * 20.0);
            // The controller initialises itself from the camera's transform,
            // and this camera has none worth reading; without this it keeps
            // its own state and ignores everything set above.
            orbit.force_update = true;

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

        // A turning camera is what the frame-time measurement wants; a still
        // one is what inspecting a joint wants. Driving the target rather than
        // the angle leaves the controller free to take over mid-turn.
        if !opts.still {
            orbit.target_yaw = opts.angle + time.elapsed_secs() * 0.35;
        }
    }
}

/// What is holding the orbit still.
///
/// The orbit shares the left button with the gizmos, the tuning panel and
/// the toggles. The panels say here whether they have the pointer, the
/// gizmos say so through [`GizmoPointer`], and [`hold_orbit`] alone switches
/// the orbit: with two writers of the switch, each would undo the other's
/// hold every frame.
#[derive(Resource, Default)]
pub struct OrbitHold {
    /// The pointer is on the tuning panel, or a slider is in hand.
    pub panel: bool,
    /// The pointer is on the toggles.
    pub toggles: bool,
}

/// Holds the orbit still while anything else has the pointer. A gizmo's
/// handle under the pointer counts as well as one in hand, so the press that
/// takes it does not also start a turn of the view.
///
/// Who has the pointer is settled when a button goes down and kept until
/// every button is up. A turn of the view that passes over a handle or the
/// panel is the view's to finish, and a drag that leaves them stays theirs.
pub(crate) fn hold_orbit(
    hold: Res<OrbitHold>,
    gizmos: Res<GizmoPointer>,
    buttons: Res<ButtonInput<MouseButton>>,
    mut settled: Local<Option<bool>>,
    mut orbits: Query<&mut PanOrbitCamera>,
) {
    let claimed = hold.panel || hold.toggles || gizmos.engaged();
    let held = if buttons.get_pressed().next().is_some() {
        *settled.get_or_insert(claimed)
    } else {
        *settled = None;
        claimed
    };
    for mut orbit in &mut orbits {
        if orbit.enabled == held {
            orbit.enabled = !held;
        }
    }
}

/// Puts a transform gizmo on the first instance once there is one, when the
/// options ask for it.
fn attach_gizmo(
    opts: Res<SceneOptions>,
    mut commands: Commands,
    instances: Query<Entity, With<arora_visuals::EntityInstance>>,
    cameras: Query<Entity, With<OrbitCamera>>,
    gizmos: Query<(), With<TransformGizmo>>,
) {
    let Some(modes) = opts.gizmo else {
        return;
    };
    if !gizmos.is_empty() {
        return;
    }
    let (Some(target), Some(camera)) = (instances.iter().next(), cameras.iter().next()) else {
        return;
    };
    commands.spawn(TransformGizmo {
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

/// Turns the orbit toward a view the view gizmo asked for: the camera moves
/// round its focus to the side the request names, at the orbit's own
/// smoothing.
///
/// Straight up or down keeps the current yaw, which is what decides how the
/// view is turned on screen there. Otherwise the yaw goes the short way round,
/// since the orbit eases its yaw toward the target as a plain number and would
/// otherwise spin the long way past a half-turn.
fn look_from(look: On<LookFrom>, mut cameras: Query<&mut PanOrbitCamera, With<OrbitCamera>>) {
    let Ok(mut orbit) = cameras.get_mut(look.camera) else {
        return;
    };
    let from = *look.from;
    orbit.target_pitch = from.y.clamp(-1.0, 1.0).asin();
    if from.y.abs() < 0.999 {
        let yaw = from.x.atan2(from.z);
        let turn = (yaw - orbit.target_yaw + PI).rem_euclid(TAU) - PI;
        orbit.target_yaw += turn;
    }
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
    materials: Query<&MeshMaterial3d<StandardMaterial>>,
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
    use std::f32::consts::FRAC_PI_2;

    use super::*;

    /// An orbit aimed by a request from the view gizmo, starting from `yaw`.
    fn aimed(yaw: f32, from: Dir3) -> (f32, f32) {
        let mut app = App::new();
        app.add_observer(look_from);
        let camera = app
            .world_mut()
            .spawn((
                PanOrbitCamera {
                    target_yaw: yaw,
                    ..default()
                },
                OrbitCamera,
            ))
            .id();
        app.world_mut().trigger(LookFrom { camera, from });
        let orbit = app.world().get::<PanOrbitCamera>(camera).unwrap();
        (orbit.target_yaw, orbit.target_pitch)
    }

    /// The orbit sits at yaw `atan2(x, z)` and pitch `asin(y)` of the side it
    /// looks from, so each axis lands on a known pair.
    #[test]
    fn each_side_lands_on_its_own_yaw_and_pitch() {
        let (yaw, pitch) = aimed(0.0, Dir3::X);
        assert!((yaw - FRAC_PI_2).abs() < 1e-5 && pitch.abs() < 1e-5);
        let (yaw, pitch) = aimed(0.0, Dir3::NEG_X);
        assert!((yaw + FRAC_PI_2).abs() < 1e-5 && pitch.abs() < 1e-5);
        let (yaw, pitch) = aimed(0.0, Dir3::Z);
        assert!(yaw.abs() < 1e-5 && pitch.abs() < 1e-5);
    }

    /// Facing behind turns the short way: from just short of a half-turn one
    /// way, the target is the half-turn on the same side, not the one a whole
    /// turn back, which the orbit's easing would spin all the way round to.
    #[test]
    fn a_turn_goes_the_short_way_round() {
        let (yaw, _) = aimed(3.0, Dir3::NEG_Z);
        assert!((yaw - PI).abs() < 1e-5, "{yaw}");
        let (yaw, _) = aimed(-3.0, Dir3::NEG_Z);
        assert!((yaw + PI).abs() < 1e-5, "{yaw}");
        let (yaw, _) = aimed(2.0 * TAU, Dir3::X);
        assert!((yaw - (2.0 * TAU + FRAC_PI_2)).abs() < 1e-4, "{yaw}");
    }

    /// Straight up or down keeps the yaw, which is what sets how the view is
    /// turned on screen there.
    #[test]
    fn looking_straight_down_keeps_the_yaw() {
        let (yaw, pitch) = aimed(0.7, Dir3::Y);
        assert!((yaw - 0.7).abs() < 1e-6 && (pitch - FRAC_PI_2).abs() < 1e-5);
        let (yaw, pitch) = aimed(0.7, Dir3::NEG_Y);
        assert!((yaw - 0.7).abs() < 1e-6 && (pitch + FRAC_PI_2).abs() < 1e-5);
    }
}
