//! A panel of sliders for tuning the stage's look by eye.
//!
//! Each slider writes straight into the current theme's [`Look`], so the
//! scene changes as the slider moves, and the panel prints the values it has
//! arrived at. A look chosen here becomes a default by copying those values
//! into [`Palette::default`] — nothing set with the panel outlives the run.
//!
//! While the pointer is over the panel or a slider is being dragged, the orbit
//! camera is held still, so dragging a slider does not also turn the view.

use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::ui_widgets::{
    Slider, SliderDragState, SliderRange, SliderStep, SliderThumb, SliderValue, TrackClick,
    ValueChange,
};
use bevy_panorbit_camera::PanOrbitCamera;

use crate::stage::{Look, Palette, Stage};
use crate::OrbitCamera;

/// What a slider controls.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
enum Knob {
    /// The ground's grey level, `#000000` to `#333333`: from black up to the
    /// dark theme's current ground, so a run starts with the thumb on the
    /// colour in use.
    Floor,
    /// The gradient's midpoint, in degrees above the horizon.
    Midpoint,
    HorizonRed,
    HorizonGreen,
    HorizonBlue,
}

impl Knob {
    fn label(self) -> &'static str {
        match self {
            Knob::Floor => "floor grey (#000000 to #333333)",
            Knob::Midpoint => "gradient midpoint (degrees)",
            Knob::HorizonRed => "horizon red",
            Knob::HorizonGreen => "horizon green",
            Knob::HorizonBlue => "horizon blue",
        }
    }

    fn range(self) -> (f32, f32, f32) {
        match self {
            Knob::Floor => (0.0, 51.0, 1.0),
            Knob::Midpoint => (0.5, 30.0, 0.1),
            Knob::HorizonRed | Knob::HorizonGreen | Knob::HorizonBlue => (0.0, 255.0, 1.0),
        }
    }

    /// The value this knob shows for a look.
    fn read(self, look: &Look) -> f32 {
        let horizon = look.horizon.to_srgba();
        match self {
            Knob::Floor => (look.ground.to_srgba().red * 255.0).round(),
            Knob::Midpoint => look.midpoint.to_degrees(),
            Knob::HorizonRed => (horizon.red * 255.0).round(),
            Knob::HorizonGreen => (horizon.green * 255.0).round(),
            Knob::HorizonBlue => (horizon.blue * 255.0).round(),
        }
    }

    /// Writes this knob's value into a look.
    fn write(self, look: &mut Look, value: f32) {
        let mut horizon = look.horizon.to_srgba();
        match self {
            Knob::Floor => {
                let grey = value / 255.0;
                look.ground = Color::srgb(grey, grey, grey);
            }
            Knob::Midpoint => look.midpoint = value.to_radians(),
            Knob::HorizonRed => horizon.red = value / 255.0,
            Knob::HorizonGreen => horizon.green = value / 255.0,
            Knob::HorizonBlue => horizon.blue = value / 255.0,
        }
        if matches!(
            self,
            Knob::HorizonRed | Knob::HorizonGreen | Knob::HorizonBlue
        ) {
            look.horizon = horizon.into();
        }
    }
}

const KNOBS: [Knob; 5] = [
    Knob::Floor,
    Knob::Midpoint,
    Knob::HorizonRed,
    Knob::HorizonGreen,
    Knob::HorizonBlue,
];

const TRACK: Color = Color::srgb(0.18, 0.18, 0.2);
const THUMB: Color = Color::srgb(0.85, 0.85, 0.9);

/// Marks a slider's thumb, which follows its value.
#[derive(Component)]
struct KnobThumb;

/// Marks the panel, whose hover holds the camera still.
#[derive(Component)]
struct TunePanel;

/// Marks the line that prints the values reached.
#[derive(Component)]
struct TuneReadout;

pub struct TunePlugin;

impl Plugin for TunePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, build_panel.after(crate::setup))
            .add_observer(on_knob)
            .add_systems(
                Update,
                (
                    follow_theme.run_if(resource_changed::<Stage>),
                    place_thumbs,
                    print_values
                        .run_if(resource_changed::<Palette>.or_else(resource_changed::<Stage>)),
                    hold_camera,
                ),
            );
    }
}

fn build_panel(
    mut commands: Commands,
    cameras: Query<Entity, With<OrbitCamera>>,
    palette: Res<Palette>,
) {
    let Ok(camera) = cameras.single() else {
        return;
    };
    let look = palette.dark;
    let text = |content: &str, size: f32| {
        (
            Text::new(content),
            TextFont {
                font_size: bevy::text::FontSize::Px(size),
                ..default()
            },
            TextColor(Color::srgb(0.9, 0.9, 0.92)),
        )
    };

    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(12.0),
                top: Val::Px(40.0),
                width: Val::Px(260.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(8.0),
                padding: UiRect::all(Val::Px(10.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.09, 0.09, 0.11, 0.9)),
            UiTargetCamera(camera),
            Hovered::default(),
            TunePanel,
        ))
        .with_children(|panel| {
            panel.spawn(text("tuning the current theme", 12.0));
            for knob in KNOBS {
                panel.spawn(text(knob.label(), 11.0));
                panel.spawn(slider(knob, knob.read(&look)));
            }
            panel.spawn((text("", 12.0), TuneReadout));
        });
}

fn slider(knob: Knob, value: f32) -> impl Bundle {
    let (start, end, step) = knob.range();
    (
        Node {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Stretch,
            height: Val::Px(14.0),
            width: Val::Px(240.0),
            ..default()
        },
        Hovered::default(),
        Slider {
            track_click: TrackClick::Snap,
            ..default()
        },
        SliderValue(value),
        SliderRange::new(start, end),
        SliderStep(step),
        knob,
        children![
            (
                Node {
                    height: Val::Px(6.0),
                    border_radius: BorderRadius::all(Val::Px(3.0)),
                    ..default()
                },
                BackgroundColor(TRACK),
            ),
            (
                // The thumb travels within this, inset by its own width so it
                // never overhangs the track's end.
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(0.0),
                    right: Val::Px(14.0),
                    top: Val::Px(0.0),
                    bottom: Val::Px(0.0),
                    ..default()
                },
                children![(
                    Node {
                        width: Val::Px(14.0),
                        height: Val::Px(14.0),
                        position_type: PositionType::Absolute,
                        left: Val::Percent(0.0),
                        border_radius: BorderRadius::MAX,
                        ..default()
                    },
                    BackgroundColor(THUMB),
                    SliderThumb,
                    KnobThumb,
                )],
            ),
        ],
    )
}

/// A slider moved: take its value, and write it into the current theme.
fn on_knob(
    change: On<ValueChange<f32>>,
    knobs: Query<&Knob>,
    stage: Res<Stage>,
    mut palette: ResMut<Palette>,
    mut commands: Commands,
) {
    let Ok(knob) = knobs.get(change.source) else {
        return;
    };
    commands
        .entity(change.source)
        .insert(SliderValue(change.value));
    knob.write(palette.look_mut(stage.theme), change.value);
}

/// When the theme flips, the sliders show the other theme's values.
fn follow_theme(
    stage: Res<Stage>,
    palette: Res<Palette>,
    knobs: Query<(Entity, &Knob)>,
    mut commands: Commands,
) {
    let look = palette.look(stage.theme);
    for (entity, knob) in &knobs {
        commands.entity(entity).insert(SliderValue(knob.read(look)));
    }
}

/// The sliders whose value moved this frame.
type MovedSliders<'w, 's> = Query<
    'w,
    's,
    (Entity, &'static SliderValue, &'static SliderRange),
    (Changed<SliderValue>, With<Knob>),
>;

fn place_thumbs(
    sliders: MovedSliders,
    children: Query<&Children>,
    mut thumbs: Query<&mut Node, With<KnobThumb>>,
) {
    for (slider, value, range) in &sliders {
        for child in children.iter_descendants(slider) {
            if let Ok(mut node) = thumbs.get_mut(child) {
                node.left = Val::Percent(range.thumb_position(value.0) * 100.0);
            }
        }
    }
}

fn print_values(
    stage: Res<Stage>,
    palette: Res<Palette>,
    mut readouts: Query<&mut Text, With<TuneReadout>>,
) {
    let look = palette.look(stage.theme);
    let hex = |colour: Color| {
        let srgb = colour.to_srgba();
        format!(
            "#{:02x}{:02x}{:02x}",
            (srgb.red * 255.0).round() as u8,
            (srgb.green * 255.0).round() as u8,
            (srgb.blue * 255.0).round() as u8
        )
    };
    for mut text in &mut readouts {
        text.0 = format!(
            "floor {}\nmidpoint {:.1} deg\nhorizon {}\nzenith {}",
            hex(look.ground),
            look.midpoint.to_degrees(),
            hex(look.horizon),
            hex(look.zenith),
        );
    }
}

/// Holds the orbit still while the pointer is on the panel or a slider is in
/// hand, so a drag moves the slider and not the view.
fn hold_camera(
    panels: Query<&Hovered, With<TunePanel>>,
    drags: Query<&SliderDragState, With<Knob>>,
    mut orbits: Query<&mut PanOrbitCamera>,
) {
    let busy = panels.iter().any(|hovered| hovered.0) || drags.iter().any(|drag| drag.dragging);
    for mut orbit in &mut orbits {
        if orbit.enabled == busy {
            orbit.enabled = !busy;
        }
    }
}
