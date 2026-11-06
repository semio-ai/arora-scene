//! The toggles in the window's corner: the stage's theme, environment and
//! lighting, and the transform gizmo's mode and axes, each a row of choices
//! with the current one lit.
//!
//! **This is the harness's, not the scene's.** A host such as Studio puts its
//! own controls for these wherever its layout wants them, and a capture shows
//! the scene alone, so the panel appears only in a window. The stage's keys
//! work alongside it; each row's label names its key.

use bevy::picking::events::PointerClick;
use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::ui::UiTargetCamera;

use arora_visuals::active_area::ActiveArea;
use arora_visuals::axes::{FrameAxes, FrameLines};
use arora_visuals::camera_input::CameraBindings;
use arora_visuals::ghost::Ghost;
use arora_visuals::joint_gizmo::JointGizmo;
use arora_visuals::transform_gizmo::{GizmoSize, GizmoSpace, TransformGizmo, TransformMode};
use arora_visuals::EntityInstance;

use crate::stage::{Environment, Lighting, Stage, Theme};
use crate::{OrbitCamera, SceneOptions};

/// The gizmo mode chosen on the panel; a key held overrides it while held.
#[derive(Resource, Default, Clone, Copy, PartialEq)]
pub struct GizmoChoice(pub Option<TransformMode>);

/// Whether the gizmos stand at a sample offset, to show that each can be
/// placed and turned relative to what it targets.
#[derive(Resource, Default, Clone, Copy, PartialEq)]
struct Shifted(bool);

/// One choice on the panel.
#[derive(Component, Clone, Copy, PartialEq)]
enum Choice {
    Theme(Theme),
    Environment(Environment),
    Lighting(Lighting),
    Mode(Option<TransformMode>),
    /// One mode the gizmo offers, switched on or off on its own, so every
    /// combination can be tried.
    Offer(TransformMode),
    Space(GizmoSpace),
    Shifted(bool),
    Ghost(bool),
    /// Wash ghosts toward the theme's ink rather than white.
    Wash(bool),
    /// A ghost's rim strength on the light theme, in tenths of Studio's.
    Rim(u8),
    /// Frame the view beside a panel covering its right side.
    Panel(bool),
    /// Let motion carry on after the input stops, as Studio's damping does.
    Inertia(bool),
    /// Keep the camera above the floor.
    Floor(bool),
    /// How the transform gizmo is sized.
    TransformSize(GizmoSize),
    /// How the joint gizmo is sized.
    JointSize(GizmoSize),
    /// Which reference frames show their axes.
    Frames(Frames),
    /// How a frame's axes are drawn.
    FrameStyle(FrameStyle),
    /// Whether a held gizmo handle shows its axis lines.
    Guides(bool),
}

/// How a frame's axes are drawn. Lines alone run one way, since nothing else
/// says which way is positive; beside the arrows they run both ways.
#[derive(Resource, Default, Clone, Copy, PartialEq, Eq, Debug)]
enum FrameStyle {
    #[default]
    Arrows,
    Lines,
    Both,
}

/// Which reference frames show their axes.
#[derive(Resource, Default, Clone, Copy, PartialEq, Eq, Debug)]
enum Frames {
    #[default]
    Off,
    /// Where the robot is placed, at Studio's body-axes length, named as
    /// Studio's Z-up world names it. Every robot has this frame, however its
    /// asset was exported; each element's own frame is shown as authored.
    Robot,
    /// Every element's frame, short enough that the robot still shows
    /// through dozens of them.
    Elements,
}

/// Each gizmo's own default, fixed in the world as Studio's are.
const TRANSFORM_IN_WORLD: GizmoSize = GizmoSize::World(0.25);
const JOINT_IN_WORLD: GizmoSize = GizmoSize::World(0.2);
/// The on-screen alternatives are each sized to match the default at the
/// opening view, where Quori shows at about 427 pixels a metre, so switching
/// changes only how a gizmo answers a zoom, not how big it starts.
const TRANSFORM_ON_SCREEN: GizmoSize = GizmoSize::Pixels(107.0);
const JOINT_ON_SCREEN: GizmoSize = GizmoSize::Pixels(85.0);

impl Choice {
    fn label(self) -> &'static str {
        match self {
            Choice::Theme(Theme::Dark) => "dark",
            Choice::Theme(Theme::Light) => "light",
            Choice::Environment(Environment::Plain) => "plain",
            Choice::Environment(Environment::Sky) => "sky",
            Choice::Lighting(Lighting::World) => "world",
            Choice::Lighting(Lighting::Camera) => "camera",
            Choice::Mode(None) => "all",
            Choice::Mode(Some(mode)) | Choice::Offer(mode) => match mode {
                TransformMode::Translate => "move",
                TransformMode::Rotate => "turn",
                TransformMode::Scale => "scale",
            },
            Choice::Space(GizmoSpace::World) => "world",
            Choice::Space(GizmoSpace::Local) => "local",
            Choice::Shifted(false) => "none",
            Choice::Shifted(true) => "shifted",
            Choice::Ghost(false) => "solid",
            Choice::Ghost(true) => "ghost",
            Choice::Wash(false) => "white",
            Choice::Wash(true) => "theme",
            Choice::Rim(10) => "1x",
            Choice::Rim(15) => "1.5x",
            Choice::Rim(20) => "2x",
            Choice::Rim(_) => "3x",
            Choice::Panel(false) => "full",
            Choice::Panel(true) => "beside panel",
            Choice::Inertia(false) => "off",
            Choice::Inertia(true) => "on",
            Choice::Floor(false) => "free",
            Choice::Frames(Frames::Off) => "off",
            Choice::Frames(Frames::Robot) => "robot",
            Choice::Frames(Frames::Elements) => "elements",
            Choice::FrameStyle(FrameStyle::Arrows) => "arrows",
            Choice::FrameStyle(FrameStyle::Lines) => "lines",
            Choice::FrameStyle(FrameStyle::Both) => "both",
            Choice::Guides(false) => "off",
            Choice::Guides(true) => "on",
            Choice::Floor(true) => "above floor",
            Choice::TransformSize(GizmoSize::Pixels(_))
            | Choice::JointSize(GizmoSize::Pixels(_)) => "on screen",
            Choice::TransformSize(GizmoSize::World(_)) | Choice::JointSize(GizmoSize::World(_)) => {
                "in world"
            }
        }
    }
}

/// The panel, so the pointer over it can hold the orbit still.
#[derive(Component)]
struct TogglePanel;

const PANEL: Color = Color::srgba(0.09, 0.09, 0.11, 0.95);
const IDLE: Color = Color::srgba(1.0, 1.0, 1.0, 0.06);
const HOVER: Color = Color::srgba(1.0, 1.0, 1.0, 0.16);
const LIT: Color = Color::srgb(0.23, 0.51, 0.96);
const INK: Color = Color::srgb(0.9, 0.9, 0.92);
const MUTED: Color = Color::srgb(0.62, 0.62, 0.66);

pub struct TogglesPlugin;

impl Plugin for TogglesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GizmoChoice>()
            .init_resource::<Shifted>()
            .init_resource::<Beside>()
            .init_resource::<Frames>()
            .init_resource::<FrameStyle>()
            .add_systems(PostStartup, (build_panel, build_strip, start_frames))
            .add_systems(
                Update,
                (show_choices, place_gizmos, frame_beside_strip, show_frames),
            );
    }
}

fn build_panel(
    mut commands: Commands,
    opts: Res<SceneOptions>,
    joint: Option<Res<crate::WantedJoint>>,
    cameras: Query<Entity, With<OrbitCamera>>,
) {
    if opts.offscreen || opts.screenshot.is_some() {
        return;
    }
    let Ok(camera) = cameras.single() else {
        return;
    };
    let mut rows: Vec<(&str, Vec<Choice>)> = vec![
        (
            "theme [T]",
            vec![Choice::Theme(Theme::Dark), Choice::Theme(Theme::Light)],
        ),
        (
            "environment [E]",
            vec![
                Choice::Environment(Environment::Plain),
                Choice::Environment(Environment::Sky),
            ],
        ),
        (
            "lighting [L]",
            vec![
                Choice::Lighting(Lighting::World),
                Choice::Lighting(Lighting::Camera),
            ],
        ),
        ("robot", vec![Choice::Ghost(false), Choice::Ghost(true)]),
        ("ghost wash", vec![Choice::Wash(false), Choice::Wash(true)]),
        (
            "ghost rim (light)",
            [10, 15, 20, 30].map(Choice::Rim).to_vec(),
        ),
        ("view", vec![Choice::Panel(false), Choice::Panel(true)]),
        (
            "inertia",
            vec![Choice::Inertia(false), Choice::Inertia(true)],
        ),
        ("camera", vec![Choice::Floor(true), Choice::Floor(false)]),
        (
            "frames",
            vec![
                Choice::Frames(Frames::Off),
                Choice::Frames(Frames::Robot),
                Choice::Frames(Frames::Elements),
            ],
        ),
        (
            "frame style",
            vec![
                Choice::FrameStyle(FrameStyle::Arrows),
                Choice::FrameStyle(FrameStyle::Lines),
                Choice::FrameStyle(FrameStyle::Both),
            ],
        ),
    ];
    if opts.gizmo.is_some() {
        const MODES: [TransformMode; 3] = [
            TransformMode::Translate,
            TransformMode::Rotate,
            TransformMode::Scale,
        ];
        rows.push(("gizmo offers", MODES.map(Choice::Offer).to_vec()));
        let mut showing = vec![Choice::Mode(None)];
        showing.extend(MODES.map(|mode| Choice::Mode(Some(mode))));
        rows.push(("gizmo shows [hold M/R/S]", showing));
        rows.push((
            "gizmo axes",
            vec![
                Choice::Space(GizmoSpace::World),
                Choice::Space(GizmoSpace::Local),
            ],
        ));
        rows.push((
            "gizmo guides",
            vec![Choice::Guides(true), Choice::Guides(false)],
        ));
        rows.push((
            "gizmo size",
            vec![
                Choice::TransformSize(TRANSFORM_IN_WORLD),
                Choice::TransformSize(TRANSFORM_ON_SCREEN),
            ],
        ));
    }
    if joint.is_some() {
        rows.push((
            "joint size",
            vec![
                Choice::JointSize(JOINT_IN_WORLD),
                Choice::JointSize(JOINT_ON_SCREEN),
            ],
        ));
    }

    if opts.gizmo.is_some() || joint.is_some() {
        rows.push((
            "gizmo offset",
            vec![Choice::Shifted(false), Choice::Shifted(true)],
        ));
    }

    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(12.0),
                top: Val::Px(12.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(6.0),
                padding: UiRect::all(Val::Px(10.0)),
                border_radius: BorderRadius::all(Val::Px(6.0)),
                ..default()
            },
            BackgroundColor(PANEL),
            UiTargetCamera(camera),
            Hovered::default(),
            TogglePanel,
        ))
        .with_children(|panel| {
            for (label, choices) in rows {
                panel
                    .spawn(Node {
                        flex_direction: FlexDirection::Row,
                        align_items: AlignItems::Center,
                        column_gap: Val::Px(4.0),
                        ..default()
                    })
                    .with_children(|row| {
                        row.spawn((
                            Text::new(label),
                            TextFont::from_font_size(11.0),
                            TextColor(MUTED),
                            Node {
                                width: Val::Px(150.0),
                                ..default()
                            },
                        ));
                        for choice in choices {
                            row.spawn((
                                choice,
                                Node {
                                    padding: UiRect::axes(Val::Px(8.0), Val::Px(3.0)),
                                    border_radius: BorderRadius::all(Val::Px(4.0)),
                                    ..default()
                                },
                                BackgroundColor(IDLE),
                                Hovered::default(),
                            ))
                            .with_child((
                                Text::new(choice.label()),
                                TextFont::from_font_size(12.0),
                                TextColor(INK),
                                Pickable::IGNORE,
                            ))
                            .observe(choose);
                        }
                    });
            }
        });
}

/// Makes the clicked choice the current one.
#[allow(clippy::too_many_arguments)]
fn choose(
    click: On<PointerClick>,
    choices: Query<&Choice>,
    mut stage: ResMut<Stage>,
    mut picked: ResMut<GizmoChoice>,
    mut shifted: ResMut<Shifted>,
    mut wash: ResMut<crate::WashChoice>,
    mut beside: ResMut<Beside>,
    instances: Query<Entity, With<EntityInstance>>,
    mut commands: Commands,
    mut gizmos: Query<&mut TransformGizmo>,
    mut joints: Query<&mut JointGizmo>,
    mut cameras: Query<&mut CameraBindings>,
    mut frames: ResMut<Frames>,
    mut style: ResMut<FrameStyle>,
) {
    let Ok(choice) = choices.get(click.entity) else {
        return;
    };
    match *choice {
        Choice::Theme(theme) => stage.theme = theme,
        Choice::Environment(environment) => stage.environment = environment,
        Choice::Lighting(lighting) => stage.lighting = lighting,
        Choice::Mode(mode) => picked.0 = mode,
        Choice::Offer(mode) => {
            for mut gizmo in &mut gizmos {
                let mut modes = gizmo.modes;
                match mode {
                    TransformMode::Translate => modes.translate = !modes.translate,
                    TransformMode::Rotate => modes.rotate = !modes.rotate,
                    TransformMode::Scale => modes.scale = !modes.scale,
                }
                // A gizmo offering nothing would vanish with no way back.
                if modes.translate || modes.rotate || modes.scale {
                    gizmo.modes = modes;
                }
            }
        }
        Choice::Space(space) => {
            for mut gizmo in &mut gizmos {
                gizmo.space = space;
            }
        }
        Choice::Shifted(on) => shifted.0 = on,
        Choice::Wash(theme) => wash.theme = theme,
        Choice::Rim(tenths) => wash.light_rim = f32::from(tenths) / 10.0,
        Choice::Panel(on) => beside.0 = on,
        Choice::Inertia(on) => {
            for mut bindings in &mut cameras {
                bindings.inertia = on;
            }
        }
        Choice::Frames(wanted) => *frames = wanted,
        Choice::FrameStyle(wanted) => *style = wanted,
        Choice::Guides(on) => {
            for mut gizmo in &mut gizmos {
                gizmo.guides = on;
            }
        }
        Choice::TransformSize(size) => {
            for mut gizmo in &mut gizmos {
                gizmo.size = size;
            }
        }
        Choice::JointSize(size) => {
            for mut joint in &mut joints {
                joint.size = size;
            }
        }
        Choice::Floor(on) => {
            for mut bindings in &mut cameras {
                bindings.floor = on.then_some(crate::CAMERA_FLOOR);
            }
        }
        Choice::Ghost(on) => {
            for instance in &instances {
                if on {
                    commands.entity(instance).insert(Ghost);
                } else {
                    commands.entity(instance).remove::<Ghost>();
                }
            }
        }
    }
}

/// Stands every gizmo at the offset the panel chose, including one spawned
/// after the choice was made.
///
/// The sample moves each gizmo off its target and turns it: the transform
/// gizmo out to one side and turned about the vertical, a joint's ring or bar
/// further from its axis and spun round it, so a ring's range turns with it.
fn place_gizmos(
    shifted: Res<Shifted>,
    mut gizmos: Query<&mut TransformGizmo>,
    mut joints: Query<&mut JointGizmo>,
) {
    for mut gizmo in &mut gizmos {
        let wanted = if shifted.0 {
            Transform::from_xyz(0.35, 0.0, 0.0).with_rotation(Quat::from_rotation_y(0.6))
        } else {
            Transform::IDENTITY
        };
        if gizmo.offset != wanted {
            gizmo.offset = wanted;
        }
    }
    for mut joint in &mut joints {
        let resting = JointGizmo::default_offset(joint.motion, joint.axis);
        let wanted = if shifted.0 {
            let outward = joint.axis.any_orthonormal_vector();
            Transform {
                translation: resting.translation + outward * 0.1,
                rotation: Quat::from_axis_angle(*joint.axis, 0.8),
                ..resting
            }
        } else {
            resting
        };
        if joint.offset != wanted {
            joint.offset = wanted;
        }
    }
}

/// Lights the current choice in each row, and the one under the pointer.
#[allow(clippy::too_many_arguments)]
fn show_choices(
    stage: Res<Stage>,
    shifted: Res<Shifted>,
    wash: Res<crate::WashChoice>,
    beside: Res<Beside>,
    instances: Query<Has<Ghost>, With<EntityInstance>>,
    gizmos: Query<&TransformGizmo>,
    joints: Query<&JointGizmo>,
    cameras: Query<&CameraBindings>,
    frames: Res<Frames>,
    style: Res<FrameStyle>,
    mut buttons: Query<(&Choice, &Hovered, &mut BackgroundColor)>,
) {
    let gizmo = gizmos.iter().next();
    let ghosted = instances.iter().any(|ghosted| ghosted);
    for (choice, hovered, mut background) in &mut buttons {
        let current = match *choice {
            Choice::Theme(theme) => stage.theme == theme,
            Choice::Environment(environment) => stage.environment == environment,
            Choice::Lighting(lighting) => stage.lighting == lighting,
            Choice::Mode(mode) => gizmo.is_some_and(|gizmo| gizmo.only == mode),
            Choice::Offer(mode) => gizmo.is_some_and(|gizmo| gizmo.modes.has(mode)),
            Choice::Space(space) => gizmo.is_some_and(|gizmo| gizmo.space == space),
            Choice::Shifted(on) => shifted.0 == on,
            Choice::Ghost(on) => ghosted == on,
            Choice::Wash(theme) => wash.theme == theme,
            Choice::Rim(tenths) => (wash.light_rim * 10.0).round() as u8 == tenths,
            Choice::Panel(on) => beside.0 == on,
            Choice::Inertia(on) => cameras.iter().any(|bindings| bindings.inertia == on),
            Choice::Floor(on) => cameras
                .iter()
                .any(|bindings| bindings.floor.is_some() == on),
            Choice::TransformSize(size) => gizmo.is_some_and(|gizmo| gizmo.size == size),
            Choice::JointSize(size) => joints.iter().any(|joint| joint.size == size),
            Choice::Frames(shown) => *frames == shown,
            Choice::FrameStyle(shown) => *style == shown,
            Choice::Guides(on) => gizmo.is_some_and(|gizmo| gizmo.guides == on),
        };
        let colour = if current {
            LIT
        } else if hovered.get() {
            HOVER
        } else {
            IDLE
        };
        if background.0 != colour {
            background.0 = colour;
        }
    }
}

/// Whether the view is framed beside the strip; the options set where it
/// starts.
#[derive(Resource, Default)]
struct Beside(bool);

/// A stand-in for a panel over the right of the canvas, as Studio's sidebar
/// covers it, so framing beside one can be seen. It takes the pointer as a
/// panel would: a press on it is the strip's, not the camera's.
#[derive(Component)]
struct Strip;

/// How wide the strip is, in logical pixels.
const STRIP_WIDTH: f32 = 380.0;

fn build_strip(
    mut commands: Commands,
    opts: Res<SceneOptions>,
    mut beside: ResMut<Beside>,
    cameras: Query<Entity, With<OrbitCamera>>,
) {
    beside.0 = opts.panel;
    let Ok(camera) = cameras.single() else {
        return;
    };
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Px(STRIP_WIDTH),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(Color::srgba(0.09, 0.09, 0.11, 0.55)),
            // Under the toggles, which sit in the same corner.
            ZIndex(-1),
            UiTargetCamera(camera),
            Hovered::default(),
            Visibility::Hidden,
            Strip,
        ))
        .with_child((
            Text::new("a panel covers this side"),
            TextFont::from_font_size(12.0),
            TextColor(MUTED),
            Pickable::IGNORE,
        ));
}

/// Starts with the frames and style the options name.
fn start_frames(
    opts: Res<SceneOptions>,
    mut frames: ResMut<Frames>,
    mut style: ResMut<FrameStyle>,
) {
    *frames = match opts.frames.as_str() {
        "robot" => Frames::Robot,
        "elements" => Frames::Elements,
        _ => Frames::Off,
    };
    *style = match opts.frame_style.as_str() {
        "lines" => FrameStyle::Lines,
        "both" => FrameStyle::Both,
        _ => FrameStyle::Arrows,
    };
}

/// Shows the chosen frames' axes in the chosen style, whenever either
/// changes or a robot's elements are joined.
fn show_frames(
    frames: Res<Frames>,
    style: Res<FrameStyle>,
    robots: Query<(Entity, &EntityInstance)>,
    joined: Query<(), Changed<EntityInstance>>,
    showing: Query<Entity, With<FrameAxes>>,
    mut commands: Commands,
) {
    if !frames.is_changed() && !style.is_changed() && joined.is_empty() {
        return;
    }
    let drawn = |axes: FrameAxes| FrameAxes {
        arrows: *style != FrameStyle::Lines,
        lines: (*style != FrameStyle::Arrows).then_some(FrameLines {
            both_ways: *style == FrameStyle::Both,
            ..default()
        }),
        ..axes
    };
    let mut wanted: Vec<(Entity, FrameAxes)> = Vec::new();
    for (robot, instance) in &robots {
        match *frames {
            Frames::Off => {}
            Frames::Robot => wanted.push((
                robot,
                drawn(FrameAxes {
                    axes: TransformGizmo::Z_UP,
                    ..default()
                }),
            )),
            Frames::Elements => {
                for targets in instance.bindings.elements.values() {
                    wanted.push((
                        targets.node,
                        drawn(FrameAxes {
                            size: GizmoSize::World(0.1),
                            ..default()
                        }),
                    ));
                }
            }
        }
    }
    // Replaced in place where a frame still shows, so its axes rebuild once
    // rather than being taken away and built again.
    for entity in &showing {
        if !wanted.iter().any(|(shown, _)| *shown == entity) {
            commands.entity(entity).remove::<FrameAxes>();
        }
    }
    for (entity, axes) in wanted {
        commands.entity(entity).insert(axes);
    }
}

/// Frames the view beside the strip, or across the whole canvas, and shows
/// the strip while it is in play.
fn frame_beside_strip(
    beside: Res<Beside>,
    mut commands: Commands,
    cameras: Query<(Entity, &Camera, Option<&ActiveArea>), With<OrbitCamera>>,
    mut strips: Query<&mut Visibility, With<Strip>>,
) {
    for (entity, camera, area) in &cameras {
        let Some(viewport) = camera.logical_viewport_size() else {
            continue;
        };
        let wanted = beside.0.then(|| {
            ActiveArea(Rect::new(
                0.0,
                0.0,
                (viewport.x - STRIP_WIDTH).max(1.0),
                viewport.y,
            ))
        });
        match (wanted, area) {
            (Some(wanted), Some(area)) if *area == wanted => {}
            (Some(wanted), _) => {
                commands.entity(entity).insert(wanted);
            }
            (None, Some(_)) => {
                commands.entity(entity).remove::<ActiveArea>();
            }
            (None, None) => {}
        }
    }
    let shown = if beside.0 {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    for mut visibility in &mut strips {
        if *visibility != shown {
            *visibility = shown;
        }
    }
}
