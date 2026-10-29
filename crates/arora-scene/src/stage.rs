//! The stage a robot stands on: what fills the frame behind it, how it is lit,
//! and how the frame is finished.
//!
//! A host makes two independent choices, each in a light and a dark variant:
//! an [`Environment`] and a [`Lighting`] rig. **The environment never lights
//! the scene.** It is what the robot is seen against, not a source of light on
//! it, so changing one never changes how the other reads.
//!
//! # Where the numbers come from
//!
//! The rigs are Studio's own, from `packages/scene`, and Studio is three.js
//! with no tone mapping. Their intensities carry across through four facts
//! rather than by eye:
//!
//! - The camera's [`Exposure`] makes Bevy's exposure factor exactly one
//!   ([`UNIT_EV100`]). Bevy scales every light by that factor; three scales by
//!   nothing.
//! - A directional light's intensity is irradiance in both engines, and both
//!   divide by π in the diffuse term, so it carries across unchanged.
//! - A point light is candela in three and lumens in Bevy, so it gains 4π.
//! - Ambient light goes through the diffuse 1/π in three and does not in Bevy,
//!   so it loses π.
//!
//! Studio's world is Z-up — its camera's up is +Z and its grid is turned to lie
//! in XY — while Bevy keeps glTF's Y-up, so Studio's positions pass through
//! [`from_studio`].
//!
//! # Why the sky has its own camera
//!
//! Bevy's atmosphere scatters *every* directional light in a view, weighted by
//! its brightness. On the main camera that makes every lamp in the rig a second
//! sun: the camera-relative key light would swing the sky round whenever the
//! camera orbits, and the sun that colours the sky would light the robot from
//! below once it set. Render layers cannot split these, because a light hidden
//! from a view is hidden from both its shading and its sky.
//!
//! So the sky is drawn by a camera of its own, beneath the main one, whose view
//! holds the sun and nothing else. The main camera holds the robot and the
//! rig, and draws over the sky without clearing it. This is the arrangement
//! Bevy's own `IndependentSkybox` uses, for a related reason.

use std::f32::consts::PI;

use bevy::anti_alias::smaa::Smaa;
use bevy::camera::visibility::RenderLayers;
use bevy::camera::{Exposure, Hdr, RenderTarget};
use bevy::core_pipeline::tonemapping::{DebandDither, Tonemapping};
use bevy::dev_tools::infinite_grid::{InfiniteGrid, InfiniteGridPlugin, InfiniteGridSettings};
use bevy::light::atmosphere::ScatteringMedium;
use bevy::light::Atmosphere;
use bevy::pbr::AtmosphereSettings;
use bevy::prelude::*;

use crate::{OrbitCamera, SceneOptions};

/// Light or dark, as the host's own theme is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

/// What fills the frame behind the robot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
pub enum Environment {
    /// A flat themed clear, with nothing competing with the robot.
    #[default]
    Plain,
    /// Bevy's atmospheric scattering. Day and night are the sun's elevation,
    /// so the theme picks one rather than choosing between two skies.
    Atmosphere,
}

/// How the robot is lit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
pub enum Lighting {
    /// Fixed in the world, and shadowed.
    #[default]
    Static,
    /// A key and a fill that travel with the camera, so the side being looked
    /// at is the lit one. Unshadowed, because a shadow that moves with the
    /// viewer reads as the object moving.
    Studio,
}

/// The `ev100` at which Bevy's exposure factor, `2^-ev100 / 1.2`, is exactly
/// one: `-log2(1.2)`.
pub const UNIT_EV100: f32 = -0.263_034_4;

/// The layer only the sun and the sky camera share.
const SKY_LAYER: usize = 31;

/// A Studio position, Z-up, as a Bevy one, Y-up.
pub fn from_studio(p: Vec3) -> Vec3 {
    Vec3::new(p.x, p.z, -p.y)
}

/// The colour a plain environment clears to — Studio's `groundColor`.
fn ground(theme: Theme) -> Color {
    match theme {
        Theme::Dark => Color::srgb_u8(0x33, 0x33, 0x33),
        Theme::Light => Color::srgb_u8(0xdd, 0xdd, 0xdd),
    }
}

/// Studio's `skyColor`, which its old environment blended toward overhead.
fn sky(theme: Theme) -> Color {
    match theme {
        Theme::Dark => Color::srgb_u8(0x11, 0x11, 0x11),
        Theme::Light => Color::srgb_u8(0xbf, 0xde, 0xfc),
    }
}

/// The light Studio's environment used to put on the robot under the static
/// rig, now owned by the rig instead.
///
/// Studio's static mode lit every material from the environment cubemap as
/// well as from its three lamps, so the lamps alone are tuned to be part of a
/// whole. Without this term the robot reads far too dark against a light
/// background; against a dark one the cubemap was near black and the
/// difference is slight. Studio's camera-relative mode already turned the
/// environment's lighting off, which is why only this rig carries it.
///
/// The cubemap was the background sphere: ground colour below the horizon and,
/// above it, the sky colour at half opacity over the ground. Averaged over the
/// sphere that is a quarter sky and three quarters ground, and a uniform
/// environment of radiance `L` puts `albedo * L` on a diffuse surface in three,
/// which is what an ambient light of colour `L` does in Bevy.
fn former_environment_light(theme: Theme) -> LinearRgba {
    let ground = LinearRgba::from(ground(theme));
    let sky = LinearRgba::from(sky(theme));
    ground * 0.75 + sky * 0.25
}

/// The static rig's point light, which is the one part of it the theme tints.
fn point_colour(theme: Theme) -> Color {
    match theme {
        Theme::Dark => Color::srgb_u8(0x17, 0x49, 0x60),
        Theme::Light => Color::srgb_u8(0xf2, 0xf7, 0x91),
    }
}

/// Where the static key light sits in Studio, which also sets the sun's
/// heading: the sky reads as lit from the side the robot is.
const STATIC_KEY: Vec3 = Vec3::new(-5.0, 5.0, 15.0);

/// The sun's elevation for a theme. A day sun is well clear of the horizon;
/// a night sun is just under it, which leaves the deep blue of twilight rather
/// than a black sky.
fn sun_elevation(theme: Theme) -> f32 {
    match theme {
        Theme::Light => 35f32.to_radians(),
        Theme::Dark => (-4f32).to_radians(),
    }
}

/// How bright the sky's sun is. It lights nothing but the sky, so this sets
/// how the sky reads against the robot rather than anything physical.
const SUN_LUX: f32 = 8.0;

/// The ground grid, in Studio's colours: one-metre cells and ten-metre
/// sections.
///
/// Studio's grid draws no axis lines, so they take the section colour here and
/// read as ordinary sections. Studio also draws sections thicker than cells
/// (1.5 against 1); Bevy's grid has one line width for both, so the two tiers
/// are told apart by colour alone.
fn grid(theme: Theme) -> InfiniteGridSettings {
    let (minor, major) = match theme {
        Theme::Dark => (
            Color::srgb_u8(0xa0, 0xa0, 0xa0),
            Color::srgb_u8(0xdc, 0xe0, 0xe8),
        ),
        Theme::Light => (
            Color::srgb_u8(0x66, 0x66, 0x66),
            Color::srgb_u8(0x28, 0x2c, 0x34),
        ),
    };
    InfiniteGridSettings {
        x_axis_color: major,
        z_axis_color: major,
        minor_line_color: minor,
        major_line_color: major,
        fadeout_distance: 100.0,
        scale: 1.0,
        major_line_interval: 10,
        ..default()
    }
}

/// Marks the camera that draws only the sky.
#[derive(Component)]
pub struct SkyCamera;

pub struct StagePlugin;

impl Plugin for StagePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(InfiniteGridPlugin)
            .add_systems(Startup, set_stage.after(crate::setup))
            .add_systems(
                PostUpdate,
                follow_main_camera.before(TransformSystems::Propagate),
            );
    }
}

/// Dresses the main camera and spawns the lights. Runs once, after the camera
/// exists.
fn set_stage(
    mut commands: Commands,
    opts: Res<SceneOptions>,
    mut cameras: Query<(Entity, &mut Camera, Option<&RenderTarget>), With<OrbitCamera>>,
    mut media: ResMut<Assets<ScatteringMedium>>,
) {
    let Ok((main, mut camera, target)) = cameras.single_mut() else {
        log::error!("no scene camera to dress");
        return;
    };
    let target = target.cloned();

    commands.entity(main).insert((
        Exposure { ev100: UNIT_EV100 },
        // Studio sets no tone mapping, so three's default — none — is the
        // look being matched. Dither off as well: 0.20 warns when the two are
        // combined, and three does not dither either.
        Tonemapping::None,
        DebandDither::Disabled,
        // Studio's composer turns multisampling off and runs SMAA last. One
        // or the other: stacking them blurs edges SMAA has already resolved.
        Msaa::Off,
        Smaa::default(),
    ));

    match opts.environment {
        Environment::Plain => {
            camera.clear_color = ClearColorConfig::Custom(ground(opts.theme));
        }
        Environment::Atmosphere => {
            // The main camera draws over the sky beneath it, so it must not
            // clear. Both are HDR so they share one target texture, which is
            // what lets the upper draw over the lower rather than replace it.
            camera.clear_color = ClearColorConfig::None;
            camera.order = 1;
            commands.entity(main).insert(Hdr);
            spawn_sky(&mut commands, &opts, target, &mut media);
        }
    }

    // On the main camera's layer, so the sky camera beneath never draws it.
    commands.spawn((InfiniteGrid, grid(opts.theme)));

    match opts.lighting {
        Lighting::Static => {
            let former = if opts.former_environment_light {
                former_environment_light(opts.theme)
            } else {
                LinearRgba::BLACK
            };
            let ambient = LinearRgba::WHITE * ((PI / 8.0) / PI) + former;
            commands.entity(main).insert(AmbientLight {
                color: ambient.into(),
                brightness: 1.0,
                ..default()
            });
            commands.spawn((
                DirectionalLight {
                    illuminance: 2.0,
                    shadow_maps_enabled: opts.shadows,
                    ..default()
                },
                Transform::from_translation(from_studio(STATIC_KEY))
                    .looking_at(Vec3::ZERO, Vec3::Y),
            ));
            commands.spawn((
                PointLight {
                    color: point_colour(opts.theme),
                    intensity: 2.0 * 4.0 * PI,
                    // Studio's is undecayed past any distance; Bevy culls at
                    // the range, and this one sits fifteen metres up.
                    range: 100.0,
                    shadow_maps_enabled: opts.shadows,
                    ..default()
                },
                Transform::from_translation(from_studio(Vec3::new(1.0, 0.0, 15.0))),
            ));
        }
        Lighting::Studio => {
            commands.entity(main).insert(AmbientLight {
                color: Color::srgb_u8(0xf5, 0xf5, 0xff),
                brightness: 0.35 / PI,
                ..default()
            });
            // Children of the camera, so the hierarchy carries them wherever it
            // goes. A directional light is a direction, so only each one's
            // rotation matters: three aims them from a position at a target,
            // and the direction between the two is what survives.
            commands.entity(main).with_children(|rig| {
                rig.spawn((
                    DirectionalLight {
                        color: Color::srgb_u8(0xff, 0xf1, 0xdf),
                        illuminance: 2.0,
                        shadow_maps_enabled: false,
                        ..default()
                    },
                    aimed(Vec3::new(-3.0, 4.0, 5.0)),
                ));
                rig.spawn((
                    DirectionalLight {
                        color: Color::srgb_u8(0xe3, 0xed, 0xff),
                        illuminance: 1.0,
                        shadow_maps_enabled: false,
                        ..default()
                    },
                    aimed(Vec3::new(4.0, 1.0, 3.0)),
                ));
            });
        }
    }
}

/// A camera-local light at `from`, aimed at Studio's rig target one unit ahead
/// of the camera. Camera space is the same in both engines — looking down -Z,
/// Y up — so this needs no axis change.
fn aimed(from: Vec3) -> Transform {
    Transform::IDENTITY.looking_to(Vec3::new(0.0, 0.0, -1.0) - from, Vec3::Y)
}

fn spawn_sky(
    commands: &mut Commands,
    opts: &SceneOptions,
    target: Option<RenderTarget>,
    media: &mut Assets<ScatteringMedium>,
) {
    commands.spawn(Atmosphere::earth(
        media.add(ScatteringMedium::earth(256, 256)),
    ));

    let heading = from_studio(STATIC_KEY)
        .with_y(0.0)
        .normalize_or(Vec3::NEG_X);
    let elevation = sun_elevation(opts.theme);
    let toward_sun = heading * elevation.cos() + Vec3::Y * elevation.sin();
    commands.spawn((
        DirectionalLight {
            illuminance: SUN_LUX,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::IDENTITY.looking_to(-toward_sun, Vec3::Y),
        RenderLayers::layer(SKY_LAYER),
    ));

    let mut sky = commands.spawn((
        Camera3d::default(),
        Camera {
            order: 0,
            ..default()
        },
        Hdr,
        AtmosphereSettings::default(),
        Exposure { ev100: UNIT_EV100 },
        Tonemapping::None,
        DebandDither::Disabled,
        Msaa::Off,
        RenderLayers::layer(SKY_LAYER),
        SkyCamera,
    ));
    if let Some(target) = target {
        sky.insert(target);
    }
}

/// The main camera's turn and lens, as the sky camera copies them.
type MainView<'w, 's> = Query<
    'w,
    's,
    (&'static Transform, &'static Projection),
    (With<OrbitCamera>, Without<SkyCamera>),
>;

/// Keeps the sky camera looking where the main one does, through the same lens.
///
/// Only the turn and the projection follow. The sky is seen from ground level
/// wherever the orbit carries the main camera, and the projection has to be
/// copied anyway, because fitting and zooming change it.
fn follow_main_camera(
    main: MainView,
    mut sky: Query<(&mut Transform, &mut Projection), With<SkyCamera>>,
) {
    let Ok((main_transform, main_projection)) = main.single() else {
        return;
    };
    for (mut transform, mut projection) in &mut sky {
        transform.rotation = main_transform.rotation;
        *projection = main_projection.clone();
    }
}
