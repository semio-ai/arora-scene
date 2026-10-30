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

use arora_visuals::joint_gizmo::JointGizmo;
use arora_visuals::transform_gizmo::{GizmoSpace, TransformGizmo, TransformMode};

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
            .add_systems(PostStartup, build_panel)
            .add_systems(
                Update,
                (
                    show_choices,
                    place_gizmos,
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
fn choose(
    click: On<PointerClick>,
    choices: Query<&Choice>,
    mut stage: ResMut<Stage>,
    mut picked: ResMut<GizmoChoice>,
    mut shifted: ResMut<Shifted>,
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
    gizmos: Query<&TransformGizmo>,
    mut buttons: Query<(&Choice, &Hovered, &mut BackgroundColor)>,
) {
    let gizmo = gizmos.iter().next();
    for (choice, hovered, mut background) in &mut buttons {
        let current = match *choice {
            Choice::Theme(theme) => stage.theme == theme,
            Choice::Environment(environment) => stage.environment == environment,
            Choice::Lighting(lighting) => stage.lighting == lighting,
            Choice::Mode(mode) => gizmo.is_some_and(|gizmo| gizmo.only == mode),
            Choice::Offer(mode) => gizmo.is_some_and(|gizmo| gizmo.modes.has(mode)),
            Choice::Space(space) => gizmo.is_some_and(|gizmo| gizmo.space == space),
            Choice::Shifted(on) => shifted.0 == on,
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

/// The pointer on the panel holds the orbit, so a click on a choice does not
/// also turn the view.
fn hold_for_panel(panels: Query<&Hovered, With<TogglePanel>>, mut hold: ResMut<OrbitHold>) {
    hold.toggles = panels.iter().any(Hovered::get);
}
