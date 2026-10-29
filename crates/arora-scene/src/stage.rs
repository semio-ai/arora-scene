//! The stage a robot stands on: what fills the frame behind it, how it is lit,
//! and how the frame is finished.
//!
//! A host makes two independent choices, each in a light and a dark variant:
//! an [`Environment`] and a [`Lighting`] anchor. **The environment never lights
//! the scene.** It is what the robot is seen against, not a source of light on
//! it, so changing one never changes how the other reads.
//!
//! **Both environments share one ground.** Below the horizon is the theme's
//! ground colour whichever environment is showing; they differ only above it.
//! That ground is a real surface rather than something each environment draws
//! its own way, because an atmosphere's ground is lit by its sky and would only
//! ever approximate the plain environment's colour.
//!
//! **Both lighting modes are the same lights.** A warm key, a cool fill and a
//! faint ambient — Studio's camera-relative rig — differing only in what they
//! are fixed to: the camera, so the side being looked at is the lit one, or
//! the world, so the light stays put while the viewer moves round it. The
//! world rig is frozen where the camera rig points from the scene's default
//! view, so the two read identically until the camera moves.
//!
//! Every choice can change while the scene runs. [`Stage`] holds the current
//! one and the scene follows it; in a window the keys listed by
//! [`StageReadout`] change it.
//!
//! # Where the numbers come from
//!
//! The rig is Studio's own, from `packages/scene`, and Studio is three.js with
//! no tone mapping. Its intensities carry across through three facts rather
//! than by eye:
//!
//! - The camera's [`Exposure`] makes Bevy's exposure factor exactly one
//!   ([`UNIT_EV100`]). Bevy scales every light by that factor; three scales by
//!   nothing.
//! - A directional light's intensity is irradiance in both engines, and both
//!   divide by π in the diffuse term, so it carries across unchanged.
//! - Ambient light goes through the diffuse 1/π in three and does not in Bevy,
//!   so it loses π.
//!
//! Camera space is the same in both engines — looking down -Z, Y up — so the
//! rig's camera-local directions need no axis change.
//!
//! # Why the sky has its own camera
//!
//! Bevy's atmosphere scatters *every* directional light in a view, weighted by
//! its brightness. On the main camera that makes every lamp in the rig a second
//! sun: a camera-fixed key light would swing the sky round whenever the camera
//! orbits, and the sun that colours the sky would light the robot from below
//! once it set. Render layers cannot split these, because a light hidden from a
//! view is hidden from both its shading and its sky.
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
use bevy::light::{Atmosphere, NotShadowCaster};
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

/// What fills the frame above the horizon.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
pub enum Environment {
    /// The ground colour all the way up, with nothing competing with the
    /// robot.
    #[default]
    Plain,
    /// Bevy's atmospheric scattering: day under a light theme, night under a
    /// dark one, set by the sun's elevation rather than by two skies.
    Atmosphere,
}

/// What the rig is fixed to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
pub enum Lighting {
    /// Fixed in the world, so the light stays put while the viewer moves round
    /// the content. The key casts shadows, which a fixed light can do sensibly.
    #[default]
    World,
    /// Fixed to the camera, so the side being looked at is the lit one.
    /// Unshadowed, because a shadow that moves with the viewer reads as the
    /// object moving.
    Camera,
}

/// The stage as it currently stands. Change it and the scene follows.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stage {
    pub theme: Theme,
    pub environment: Environment,
    pub lighting: Lighting,
}

/// The `ev100` at which Bevy's exposure factor, `2^-ev100 / 1.2`, is exactly
/// one: `-log2(1.2)`.
pub const UNIT_EV100: f32 = -0.263_034_4;

/// The layer only the sun and the sky camera share.
const SKY_LAYER: usize = 31;

/// How far below the grid the shared ground sits. Far enough that no robot's
/// feet reach it — the ground is unlit and flat, so its depth is invisible —
/// while the grid still draws at zero.
const GROUND_DEPTH: f32 = -1.0;

/// How far the ground extends, and how far the main camera must see to reach
/// its edge. A flat plane meets the horizon only at infinity, so whatever lies
/// past its edge shows as a band just under the horizon; at this distance the
/// band is a fraction of a pixel from any height the orbit allows.
pub const GROUND_REACH: f32 = 100_000.0;

/// The ground colour — Studio's `groundColor`, which it also cleared to.
fn ground(theme: Theme) -> Color {
    match theme {
        Theme::Dark => Color::srgb_u8(0x33, 0x33, 0x33),
        Theme::Light => Color::srgb_u8(0xdd, 0xdd, 0xdd),
    }
}

/// One directional light of the rig: its colour, its illuminance, and where
/// Studio places it in camera space.
struct RigLamp {
    colour: Color,
    lux: f32,
    from: Vec3,
    /// Whether it casts shadows when fixed in the world.
    shadows: bool,
}

/// Studio's camera-relative rig: a warm key above and to the left, a cool fill
/// low and to the right, both aimed one unit ahead of the camera.
fn rig() -> [RigLamp; 2] {
    [
        RigLamp {
            colour: Color::srgb_u8(0xff, 0xf1, 0xdf),
            lux: 2.0,
            from: Vec3::new(-3.0, 4.0, 5.0),
            shadows: true,
        },
        RigLamp {
            colour: Color::srgb_u8(0xe3, 0xed, 0xff),
            lux: 1.0,
            from: Vec3::new(4.0, 1.0, 3.0),
            shadows: false,
        },
    ]
}

/// The rig's ambient term, the same under either anchor.
fn rig_ambient() -> AmbientLight {
    AmbientLight {
        color: Color::srgb_u8(0xf5, 0xf5, 0xff),
        brightness: 0.35 / PI,
        ..default()
    }
}

/// A light at camera-space `from`, aimed at Studio's rig target one unit ahead
/// of the camera. Only the rotation matters to a directional light: three aims
/// it from a position at a target, and the direction between the two is what
/// survives.
fn aimed(from: Vec3) -> Transform {
    Transform::IDENTITY.looking_to(Vec3::new(0.0, 0.0, -1.0) - from, Vec3::Y)
}

/// The camera's orientation at the scene's default view: the yaw and pitch the
/// orbit starts from, composed as the orbit controller composes them.
fn default_view(opts: &SceneOptions) -> Quat {
    Quat::from_rotation_y(opts.angle) * Quat::from_rotation_x(-opts.pitch)
}

/// The sun's elevation for a theme. A day sun is well clear of the horizon; a
/// night sun is far enough below it that the twilight has gone too.
fn sun_elevation(theme: Theme) -> f32 {
    match theme {
        Theme::Light => 35f32.to_radians(),
        Theme::Dark => (-20f32).to_radians(),
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

/// Marks the light that colours the sky and lights nothing else.
#[derive(Component)]
struct Sun;

/// Marks the shared ground, which framing and the scene's census leave out:
/// it is the stage, not the thing on it.
#[derive(Component)]
pub struct Ground;

/// The world-fixed frame the rig hangs from under [`Lighting::World`]: the
/// camera's orientation at the default view, so the rig starts where the
/// camera rig would be and then stays.
#[derive(Component)]
struct RigAnchor;

/// Marks a light belonging to the current rig, so a change of anchor can take
/// it away.
#[derive(Component)]
struct RigLight;

/// The on-screen line naming the current stage and the keys that change it.
#[derive(Component)]
pub struct StageReadout;

pub struct StagePlugin;

impl Plugin for StagePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(InfiniteGridPlugin)
            .add_systems(Startup, build_stage.after(crate::setup))
            .add_systems(
                Update,
                (
                    stage_keys,
                    (apply_environment, apply_lighting, apply_theme, show_stage)
                        .run_if(resource_changed::<Stage>),
                )
                    .chain(),
            )
            .add_systems(PostUpdate, follow_main_lens);
    }
}

/// Builds everything the stage keeps for the whole run, whichever choices are
/// current: the camera's finish, the sky camera and its sun, the atmosphere,
/// the ground, the grid and the world rig's anchor. What the choices change is
/// applied separately, so a change at runtime takes the same path as the
/// first frame.
fn build_stage(
    mut commands: Commands,
    opts: Res<SceneOptions>,
    mut cameras: Query<(Entity, &mut Camera, Option<&RenderTarget>), With<OrbitCamera>>,
    mut media: ResMut<Assets<ScatteringMedium>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let Ok((main, mut camera, target)) = cameras.single_mut() else {
        log::error!("no scene camera to dress");
        return;
    };
    let target = target.cloned();

    // Above the sky camera, so it draws over whatever the sky put down.
    camera.order = 1;
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
        // Shared with the sky camera, so the two write one target texture and
        // the upper can draw over the lower rather than replace it. Kept on in
        // the plain environment too, so switching never re-specialises the
        // camera's pipelines.
        Hdr,
        rig_ambient(),
    ));

    commands.spawn(Atmosphere::earth(
        media.add(ScatteringMedium::earth(256, 256)),
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: SUN_LUX,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::IDENTITY,
        RenderLayers::layer(SKY_LAYER),
        Sun,
    ));
    // A child of the main camera, so the transform hierarchy turns the two
    // together. Copying the turn across instead lags the orbit by a frame
    // whenever the copy happens to run first, and on a fast orbit that frame
    // opens a band of the atmosphere's own ground between sky and floor.
    let mut sky = commands.spawn((
        Camera3d::default(),
        Camera {
            order: 0,
            is_active: false,
            ..default()
        },
        Hdr,
        AtmosphereSettings::default(),
        Exposure { ev100: UNIT_EV100 },
        Tonemapping::None,
        DebandDither::Disabled,
        Msaa::Off,
        RenderLayers::layer(SKY_LAYER),
        Transform::IDENTITY,
        ChildOf(main),
        SkyCamera,
    ));
    if let Some(target) = target {
        sky.insert(target);
    }

    commands.spawn((
        Transform::from_rotation(default_view(&opts)),
        Visibility::default(),
        RigAnchor,
    ));

    // Unlit, so it is exactly the ground colour from every angle and under
    // every rig — the same colour the plain environment clears to. Drawn by
    // the main camera rather than the sky camera: in the sky camera's view the
    // atmosphere would haze it with distance, and it would match nothing.
    commands.spawn((
        Mesh3d(
            meshes.add(
                Plane3d::default()
                    .mesh()
                    .size(2.0 * GROUND_REACH, 2.0 * GROUND_REACH),
            ),
        ),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: ground(opts.theme),
            unlit: true,
            ..default()
        })),
        Transform::from_xyz(0.0, GROUND_DEPTH, 0.0),
        NotShadowCaster,
        Ground,
    ));

    // On the main camera's layer, so the sky camera beneath never draws it.
    commands.spawn((InfiniteGrid, grid(opts.theme)));

    // A capture wants the scene alone; a window wants to know what it is
    // looking at and how to change it.
    if !opts.offscreen && opts.screenshot.is_none() {
        commands.spawn((
            Text::new(""),
            TextFont {
                font_size: bevy::text::FontSize::Px(13.0),
                ..default()
            },
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(12.0),
                top: Val::Px(10.0),
                padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.09, 0.09, 0.11, 0.85)),
            UiTargetCamera(main),
            StageReadout,
        ));
    }

    commands.insert_resource(Stage {
        theme: opts.theme,
        environment: opts.environment,
        lighting: opts.lighting,
    });
}

/// `E` environment, `L` lighting, `T` theme.
fn stage_keys(keys: Res<ButtonInput<KeyCode>>, mut stage: ResMut<Stage>) {
    if keys.just_pressed(KeyCode::KeyE) {
        stage.environment = match stage.environment {
            Environment::Plain => Environment::Atmosphere,
            Environment::Atmosphere => Environment::Plain,
        };
    }
    if keys.just_pressed(KeyCode::KeyL) {
        stage.lighting = match stage.lighting {
            Lighting::World => Lighting::Camera,
            Lighting::Camera => Lighting::World,
        };
    }
    if keys.just_pressed(KeyCode::KeyT) {
        stage.theme = match stage.theme {
            Theme::Dark => Theme::Light,
            Theme::Light => Theme::Dark,
        };
    }
}

/// Shows the sky or the plain ground colour above the horizon.
fn apply_environment(
    stage: Res<Stage>,
    mut main: Query<&mut Camera, (With<OrbitCamera>, Without<SkyCamera>)>,
    mut sky: Query<&mut Camera, With<SkyCamera>>,
) {
    let plain = stage.environment == Environment::Plain;
    for mut camera in &mut main {
        camera.clear_color = if plain {
            ClearColorConfig::Custom(ground(stage.theme))
        } else {
            // The sky beneath has already filled the target.
            ClearColorConfig::None
        };
    }
    for mut camera in &mut sky {
        camera.is_active = !plain;
    }
}

/// Hangs the rig from the camera or from the world anchor, replacing whatever
/// hung there before.
fn apply_lighting(
    stage: Res<Stage>,
    opts: Res<SceneOptions>,
    mut commands: Commands,
    main: Query<Entity, (With<OrbitCamera>, Without<SkyCamera>)>,
    anchor: Query<Entity, With<RigAnchor>>,
    old: Query<Entity, With<RigLight>>,
) {
    let parent = match stage.lighting {
        Lighting::Camera => main.single(),
        Lighting::World => anchor.single(),
    };
    let Ok(parent) = parent else {
        return;
    };
    for light in &old {
        commands.entity(light).despawn();
    }

    let fixed = stage.lighting == Lighting::World;
    commands.entity(parent).with_children(|hanging| {
        for lamp in rig() {
            hanging.spawn((
                DirectionalLight {
                    color: lamp.colour,
                    illuminance: lamp.lux,
                    shadow_maps_enabled: fixed && lamp.shadows && opts.shadows,
                    ..default()
                },
                aimed(lamp.from),
                RigLight,
            ));
        }
    });
}

/// Recolours what the theme touches: the ground, the grid, the sun's height
/// and the atmosphere's idea of the ground beneath it.
fn apply_theme(
    stage: Res<Stage>,
    opts: Res<SceneOptions>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    grounds: Query<&MeshMaterial3d<StandardMaterial>, With<Ground>>,
    mut grids: Query<&mut InfiniteGridSettings, With<InfiniteGrid>>,
    mut suns: Query<&mut Transform, With<Sun>>,
    mut atmospheres: Query<&mut Atmosphere>,
) {
    for handle in &grounds {
        if let Some(mut material) = materials.get_mut(&handle.0) {
            material.base_color = ground(stage.theme);
        }
    }
    for mut settings in &mut grids {
        *settings = grid(stage.theme);
    }

    // The sun shares the world rig key's heading, so the sky is brightest on
    // the side the robot is lit from.
    let key = rig()[0].from;
    let toward_key = default_view(&opts) * (key - Vec3::new(0.0, 0.0, -1.0));
    let heading = toward_key.with_y(0.0).normalize_or(Vec3::NEG_X);
    let elevation = sun_elevation(stage.theme);
    let toward_sun = heading * elevation.cos() + Vec3::Y * elevation.sin();
    for mut transform in &mut suns {
        *transform = Transform::IDENTITY.looking_to(-toward_sun, Vec3::Y);
    }

    // The visible ground is the shared surface, but the atmosphere still
    // bounces light off a ground of its own when it scatters. Giving it the
    // same colour keeps the horizon from tinting toward a planet that is not
    // there.
    let albedo = LinearRgba::from(ground(stage.theme));
    for mut atmosphere in &mut atmospheres {
        atmosphere.ground_albedo = Vec3::new(albedo.red, albedo.green, albedo.blue);
    }
}

fn show_stage(stage: Res<Stage>, mut readouts: Query<&mut Text, With<StageReadout>>) {
    for mut text in &mut readouts {
        text.0 = format!(
            "[E] environment: {:?}   [L] lighting: {:?}   [T] theme: {:?}",
            stage.environment, stage.lighting, stage.theme,
        );
    }
}

/// Gives the sky camera the main camera's lens. Only the projection needs
/// copying — the hierarchy already turns the two together — and it changes
/// only when the camera is fitted, so a frame's lag here is never visible.
fn follow_main_lens(
    main: Query<&Projection, (With<OrbitCamera>, Without<SkyCamera>)>,
    mut sky: Query<&mut Projection, With<SkyCamera>>,
) {
    let Ok(lens) = main.single() else {
        return;
    };
    for mut projection in &mut sky {
        *projection = lens.clone();
    }
}

#[cfg(test)]
mod tests {
    use bevy::ecs::system::RunSystemOnce;

    use super::*;

    /// A world with the two cameras and the anchor the stage dresses, and
    /// nothing rendering.
    fn world(stage: Stage) -> (World, Entity, Entity, Entity) {
        let mut world = World::new();
        world.insert_resource(SceneOptions::default());
        world.insert_resource(stage);
        let main = world.spawn((Camera::default(), OrbitCamera)).id();
        let sky = world
            .spawn((
                Camera {
                    is_active: false,
                    ..default()
                },
                SkyCamera,
            ))
            .id();
        let anchor = world.spawn((Transform::IDENTITY, RigAnchor)).id();
        (world, main, sky, anchor)
    }

    fn stage(environment: Environment, lighting: Lighting, theme: Theme) -> Stage {
        Stage {
            theme,
            environment,
            lighting,
        }
    }

    fn hung(world: &mut World) -> Vec<(Option<Entity>, DirectionalLight, Transform)> {
        let mut query = world
            .query_filtered::<(Option<&ChildOf>, &DirectionalLight, &Transform), With<RigLight>>();
        query
            .iter(world)
            .map(|(parent, light, transform)| {
                (parent.map(ChildOf::parent), *light, *transform)
            })
            .collect()
    }

    #[test]
    fn each_environment_shows_the_ground_or_the_sky_and_switches_back() {
        let (mut world, main, sky, _) =
            world(stage(Environment::Plain, Lighting::World, Theme::Dark));
        world.run_system_once(apply_environment).unwrap();
        assert!(
            matches!(world.get::<Camera>(main).unwrap().clear_color,
                ClearColorConfig::Custom(colour) if colour == ground(Theme::Dark)),
            "plain clears to the ground colour",
        );
        assert!(
            !world.get::<Camera>(sky).unwrap().is_active,
            "no sky under plain"
        );

        world.resource_mut::<Stage>().environment = Environment::Atmosphere;
        world.run_system_once(apply_environment).unwrap();
        assert!(
            matches!(
                world.get::<Camera>(main).unwrap().clear_color,
                ClearColorConfig::None
            ),
            "over a sky the main camera must not clear what the sky drew",
        );
        assert!(world.get::<Camera>(sky).unwrap().is_active);

        let mut back = world.resource_mut::<Stage>();
        back.environment = Environment::Plain;
        back.theme = Theme::Light;
        world.run_system_once(apply_environment).unwrap();
        assert!(matches!(world.get::<Camera>(main).unwrap().clear_color,
            ClearColorConfig::Custom(colour) if colour == ground(Theme::Light)));
        assert!(!world.get::<Camera>(sky).unwrap().is_active);
    }

    /// A rig left behind by a switch keeps lighting the robot, so the count is
    /// asserted after every change rather than only the new lights' presence.
    #[test]
    fn switching_anchor_moves_the_whole_rig() {
        let (mut world, main, _, anchor) =
            world(stage(Environment::Plain, Lighting::World, Theme::Dark));

        world.run_system_once(apply_lighting).unwrap();
        let fixed = hung(&mut world);
        assert_eq!(fixed.len(), 2, "a key and a fill");
        assert!(fixed.iter().all(|(parent, ..)| *parent == Some(anchor)));

        world.resource_mut::<Stage>().lighting = Lighting::Camera;
        world.run_system_once(apply_lighting).unwrap();
        let riding = hung(&mut world);
        assert_eq!(riding.len(), 2, "the world rig is gone, not added to");
        assert!(riding.iter().all(|(parent, ..)| *parent == Some(main)));

        world.resource_mut::<Stage>().lighting = Lighting::World;
        world.run_system_once(apply_lighting).unwrap();
        assert_eq!(hung(&mut world).len(), 2);
        assert!(
            world
                .get::<Children>(main)
                .is_none_or(|children| children.is_empty()),
            "nothing is left hanging off the camera",
        );
    }

    /// The two anchors hold the same lights: same colours, same strengths, same
    /// directions relative to what they hang from. Only shadows differ.
    #[test]
    fn both_anchors_carry_the_same_lights() {
        let lamps = |lighting| {
            let (mut world, ..) = world(stage(Environment::Plain, lighting, Theme::Dark));
            world.run_system_once(apply_lighting).unwrap();
            let mut lamps = hung(&mut world);
            lamps.sort_by(|a, b| b.1.illuminance.total_cmp(&a.1.illuminance));
            lamps
        };
        let fixed = lamps(Lighting::World);
        let riding = lamps(Lighting::Camera);
        for ((_, world_lamp, world_turn), (_, camera_lamp, camera_turn)) in
            fixed.iter().zip(&riding)
        {
            assert_eq!(world_lamp.color, camera_lamp.color);
            assert_eq!(world_lamp.illuminance, camera_lamp.illuminance);
            assert_eq!(world_turn.rotation, camera_turn.rotation);
            assert!(
                !camera_lamp.shadow_maps_enabled,
                "nothing riding the camera casts"
            );
        }
        assert!(fixed[0].1.shadow_maps_enabled, "the fixed key casts");
        assert!(!fixed[1].1.shadow_maps_enabled, "the fill does not");
    }
}
