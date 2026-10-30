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

use arora_visuals::transform_gizmo::{GizmoSpace, TransformGizmo, TransformMode};

use crate::stage::{Environment, Lighting, Stage, Theme};
use crate::{OrbitCamera, OrbitHold, SceneOptions};

/// The gizmo mode chosen on the panel; a key held overrides it while held.
#[derive(Resource, Default, Clone, Copy, PartialEq)]
pub struct GizmoChoice(pub Option<TransformMode>);

/// One choice on the panel.
#[derive(Component, Clone, Copy, PartialEq)]
enum Choice {
    Theme(Theme),
    Environment(Environment),
    Lighting(Lighting),
    Mode(Option<TransformMode>),
    Space(GizmoSpace),
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
            Choice::Mode(Some(TransformMode::Translate)) => "move",
            Choice::Mode(Some(TransformMode::Rotate)) => "turn",
            Choice::Mode(Some(TransformMode::Scale)) => "scale",
            Choice::Space(GizmoSpace::World) => "world",
            Choice::Space(GizmoSpace::Local) => "local",
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
            .add_systems(PostStartup, build_panel)
            .add_systems(
                Update,
                (show_choices, hold_for_panel.before(crate::hold_orbit)),
            );
    }
}

fn build_panel(
    mut commands: Commands,
    opts: Res<SceneOptions>,
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
    if let Some(modes) = opts.gizmo {
        let mut choices = vec![Choice::Mode(None)];
        for mode in [
            TransformMode::Translate,
            TransformMode::Rotate,
            TransformMode::Scale,
        ] {
            if modes.has(mode) {
                choices.push(Choice::Mode(Some(mode)));
            }
        }
        rows.push(("gizmo [hold M/R/S]", choices));
        rows.push((
            "gizmo axes",
            vec![
                Choice::Space(GizmoSpace::World),
                Choice::Space(GizmoSpace::Local),
            ],
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
                                width: Val::Px(136.0),
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
        Choice::Space(space) => {
            for mut gizmo in &mut gizmos {
                gizmo.space = space;
            }
        }
    }
}

/// Lights the current choice in each row, and the one under the pointer.
fn show_choices(
    stage: Res<Stage>,
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
            Choice::Space(space) => gizmo.is_some_and(|gizmo| gizmo.space == space),
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
