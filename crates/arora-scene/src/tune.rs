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
    Slider, SliderRange, SliderStep, SliderThumb, SliderValue, TrackClick, ValueChange,
};

use crate::aurora::Aurora;
use crate::stage::{Look, Palette, Stage};
use crate::OrbitCamera;

/// What a slider controls.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
enum Knob {
    /// The ground's grey level, `#000000` to `#333333`: black up to Studio's
    /// own dark ground, the range a dark theme's floor is worth trying in.
    Floor,
    /// The gradient's midpoint, in degrees above the horizon.
    Midpoint,
    HorizonRed,
    HorizonGreen,
    HorizonBlue,
    AuroraBrightness,
    /// In metres.
    AuroraWave,
    /// In cycles a second.
    AuroraSpeed,
    /// In metres.
    AuroraAltitude,
    /// From none to full.
    AuroraEdgeFade,
    /// In metres.
    AuroraColourScale,
    /// In changes a second.
    AuroraColourSpeed,
    AuroraColourLift,
}

impl Knob {
    fn label(self) -> &'static str {
        match self {
            Knob::Floor => "floor grey (#000000 to #ffffff)",
            Knob::Midpoint => "gradient midpoint (degrees)",
            Knob::HorizonRed => "horizon red",
            Knob::HorizonGreen => "horizon green",
            Knob::HorizonBlue => "horizon blue",
            Knob::AuroraBrightness => "aurora brightness",
            Knob::AuroraWave => "aurora wave (m)",
            Knob::AuroraSpeed => "aurora speed (cycles/s)",
            Knob::AuroraAltitude => "aurora altitude (m)",
            Knob::AuroraEdgeFade => "aurora edge-on fade",
            Knob::AuroraColourScale => "aurora colour patch (m)",
            Knob::AuroraColourSpeed => "aurora colour speed (changes/s)",
            Knob::AuroraColourLift => "aurora colour lift toward the top",
        }
    }

    fn range(self) -> (f32, f32, f32) {
        match self {
            // The whole range, so either theme's floor sits within it.
            Knob::Floor => (0.0, 255.0, 1.0),
            Knob::Midpoint => (0.5, 30.0, 0.1),
            Knob::HorizonRed | Knob::HorizonGreen | Knob::HorizonBlue => (0.0, 255.0, 1.0),
            Knob::AuroraBrightness => (0.0, 2.0, 0.01),
            Knob::AuroraWave => (0.0, 300.0, 1.0),
            Knob::AuroraSpeed => (0.0, 0.05, 0.0005),
            Knob::AuroraAltitude => (100.0, 2000.0, 10.0),
            Knob::AuroraEdgeFade => (0.0, 1.0, 0.01),
            Knob::AuroraColourScale => (50.0, 3000.0, 10.0),
            Knob::AuroraColourSpeed => (0.0, 0.5, 0.005),
            Knob::AuroraColourLift => (0.0, 1.0, 0.01),
        }
    }

    /// The value this knob shows.
    fn read(self, look: &Look, aurora: &Aurora) -> f32 {
        let horizon = look.horizon.to_srgba();
        match self {
            Knob::AuroraBrightness => aurora.brightness,
            Knob::AuroraWave => aurora.wave,
            Knob::AuroraSpeed => aurora.speed,
            Knob::AuroraAltitude => aurora.altitude,
            Knob::AuroraEdgeFade => aurora.edge_fade,
            Knob::AuroraColourScale => aurora.colour_scale,
            Knob::AuroraColourSpeed => aurora.colour_speed,
            Knob::AuroraColourLift => aurora.colour_lift,
            Knob::Floor => (look.ground.to_srgba().red * 255.0).round(),
            Knob::Midpoint => look.midpoint.to_degrees(),
            Knob::HorizonRed => (horizon.red * 255.0).round(),
            Knob::HorizonGreen => (horizon.green * 255.0).round(),
            Knob::HorizonBlue => (horizon.blue * 255.0).round(),
        }
    }

    /// Writes this knob's value where it belongs.
    fn write(self, look: &mut Look, aurora: &mut Aurora, value: f32) {
        let mut horizon = look.horizon.to_srgba();
        match self {
            Knob::AuroraBrightness => aurora.brightness = value,
            Knob::AuroraWave => aurora.wave = value,
            Knob::AuroraSpeed => aurora.speed = value,
            Knob::AuroraAltitude => aurora.altitude = value,
            Knob::AuroraEdgeFade => aurora.edge_fade = value,
            Knob::AuroraColourScale => aurora.colour_scale = value,
            Knob::AuroraColourSpeed => aurora.colour_speed = value,
            Knob::AuroraColourLift => aurora.colour_lift = value,
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

const KNOBS: [Knob; 13] = [
    Knob::Floor,
    Knob::Midpoint,
    Knob::HorizonRed,
    Knob::HorizonGreen,
    Knob::HorizonBlue,
    Knob::AuroraBrightness,
    Knob::AuroraWave,
    Knob::AuroraSpeed,
    Knob::AuroraAltitude,
    Knob::AuroraEdgeFade,
    Knob::AuroraColourScale,
    Knob::AuroraColourSpeed,
    Knob::AuroraColourLift,
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
                    print_values.run_if(
                        resource_changed::<Palette>
                            .or_else(resource_changed::<Stage>)
                            .or_else(resource_changed::<Aurora>),
                    ),
                ),
            );
    }
}

fn build_panel(
    mut commands: Commands,
    cameras: Query<Entity, With<OrbitCamera>>,
    palette: Res<Palette>,
    aurora: Res<Aurora>,
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
                panel.spawn(slider(knob, knob.read(&look, &aurora)));
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
    mut aurora: ResMut<Aurora>,
    mut commands: Commands,
) {
    let Ok(knob) = knobs.get(change.source) else {
        return;
    };
    commands
        .entity(change.source)
        .insert(SliderValue(change.value));
    knob.write(palette.look_mut(stage.theme), &mut aurora, change.value);
}

/// When the theme flips, the sliders show the other theme's values.
fn follow_theme(
    stage: Res<Stage>,
    palette: Res<Palette>,
    aurora: Res<Aurora>,
    knobs: Query<(Entity, &Knob)>,
    mut commands: Commands,
) {
    let look = palette.look(stage.theme);
    for (entity, knob) in &knobs {
        commands
            .entity(entity)
            .insert(SliderValue(knob.read(look, &aurora)));
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
    aurora: Res<Aurora>,
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
            "floor {}\nmidpoint {:.1} deg\nhorizon {}\nzenith {}\n\
             aurora brightness {:.2}, wave {:.0} m, speed {:.4}/s, altitude {:.0} m, edge fade {:.2}\n\
             aurora colour patch {:.0} m, colour speed {:.3}/s, colour lift {:.2}",
            hex(look.ground),
            look.midpoint.to_degrees(),
            hex(look.horizon),
            hex(look.zenith),
            aurora.brightness,
            aurora.wave,
            aurora.speed,
            aurora.altitude,
            aurora.edge_fade,
            aurora.colour_scale,
            aurora.colour_speed,
            aurora.colour_lift,
        );
    }
}
