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
use arora_visuals::ghost::Ghost;
use arora_visuals::joint_gizmo::JointGizmo;
use arora_visuals::transform_gizmo::{GizmoSpace, TransformGizmo, TransformMode};
use arora_visuals::EntityInstance;

use crate::stage::{Environment, Lighting, Stage, Theme};
use crate::{OrbitCamera, OrbitHold, SceneOptions};

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
}

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
            .add_systems(PostStartup, (build_panel, build_strip))
            .add_systems(
                Update,
                (
                    show_choices,
                    place_gizmos,
                    frame_beside_strip,
                    hold_for_panel.before(crate::hold_orbit),
                ),
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
fn show_choices(
    stage: Res<Stage>,
    shifted: Res<Shifted>,
    wash: Res<crate::WashChoice>,
    beside: Res<Beside>,
    instances: Query<Has<Ghost>, With<EntityInstance>>,
    gizmos: Query<&TransformGizmo>,
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

/// The panels whose hover holds the orbit: the toggles and the strip.
type HoldingPanels<'w, 's> = Query<'w, 's, &'static Hovered, Or<(With<TogglePanel>, With<Strip>)>>;

/// The pointer on the panel or the strip holds the orbit, so a click there
/// does not also turn the view.
fn hold_for_panel(panels: HoldingPanels, mut hold: ResMut<OrbitHold>) {
    hold.toggles = panels.iter().any(Hovered::get);
}

/// Whether the view is framed beside the strip; the options set where it
/// starts.
#[derive(Resource, Default)]
struct Beside(bool);

/// A stand-in for a panel over the right of the canvas, as Studio's sidebar
/// covers it, so framing beside one can be seen. It takes the pointer as a
/// panel would.
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
