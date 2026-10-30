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
//! The floor is that colour wherever it is lit, and darkens only where a
//! shadow falls, so a robot stands on it rather than floating over a grid.
//!
//! **The sky is a gradient, not a simulation.** Day is a blue gradient over a
//! pale horizon, night a deep blue one with stars. The sun never moves, so a
//! physically scattered sky would spend a second camera, lookup textures and a
//! light of its own to arrive at a fixed picture — and that picture is only
//! ever as close to the intended one as the physics allows. A gradient is the
//! intended picture directly, at the cost of one dome.
//!
//! **Both lighting modes are the same lights.** A warm key, a cool fill and a
//! faint ambient — Studio's camera-relative rig — differing only in what they
//! are fixed to: the camera, so the side being looked at is the lit one, or
//! the world, so the light stays put while the viewer moves round it. The
//! world rig is frozen where the camera rig points from the scene's default
//! view, so the two read identically until the camera moves. The key casts
//! shadows under either.
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

use std::f32::consts::PI;

use bevy::anti_alias::smaa::Smaa;
use bevy::asset::{embedded_asset, embedded_path, AssetPath};
use bevy::camera::Exposure;
use bevy::core_pipeline::tonemapping::{DebandDither, Tonemapping};
use bevy::dev_tools::infinite_grid::{InfiniteGrid, InfiniteGridPlugin, InfiniteGridSettings};
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::pbr::{Material, MaterialPlugin};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;

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
    /// A day sky under a light theme, a night sky with stars under a dark one.
    Sky,
}

/// What the rig is fixed to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
pub enum Lighting {
    /// Fixed in the world, so the light stays put while the viewer moves round
    /// the content.
    #[default]
    World,
    /// Fixed to the camera, so the side being looked at is the lit one.
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

/// How far the main camera must see: past the floor's edge and out to the sky
/// dome. A flat floor meets the horizon only at infinity, so at a shorter reach
/// a band of whatever lies beyond would show beneath the horizon.
pub const GROUND_REACH: f32 = 100_000.0;

/// The sky dome's radius, inside the camera's reach so it is never clipped.
const DOME_RADIUS: f32 = GROUND_REACH * 0.5;

/// The floor's height. A hair under the grid, which draws at zero, so the two
/// never fight over the same depth.
const FLOOR_HEIGHT: f32 = -0.002;

/// How far a full shadow darkens the floor.
const FLOOR_SHADOW: f32 = 0.35;

/// What one theme is drawn in. Live, so a host — or the tuning panel — can
/// change any of it and the scene follows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    /// The floor, the mountains, and the plain environment top to bottom.
    pub ground: Color,
    pub zenith: Color,
    pub horizon: Color,
    /// How high above the horizon, in radians, the sky is halfway from the
    /// horizon colour to the zenith's. Lower keeps the horizon colour to a
    /// thinner band.
    pub midpoint: f32,
    /// How strongly stars show, from none to full.
    pub stars: f32,
}

/// The look of each theme.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub dark: Look,
    pub light: Look,
}

impl Palette {
    pub fn look(&self, theme: Theme) -> &Look {
        match theme {
            Theme::Dark => &self.dark,
            Theme::Light => &self.light,
        }
    }

    pub fn look_mut(&mut self, theme: Theme) -> &mut Look {
        match theme {
            Theme::Dark => &mut self.dark,
            Theme::Light => &mut self.light,
        }
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            // A near-black ground with the hills as a dark silhouette, a thin
            // grey glow along them, and the rest of the night the zenith's
            // near-black. Set by eye with the tuning panel.
            dark: Look {
                ground: Color::srgb_u8(0x17, 0x17, 0x17),
                zenith: Color::srgb_u8(0x03, 0x05, 0x0c),
                horizon: Color::srgb_u8(0x55, 0x55, 0x55),
                midpoint: 0.017_453_3,
                stars: 1.0,
            },
            light: Look {
                ground: Color::srgb_u8(0xdd, 0xdd, 0xdd),
                zenith: Color::srgb_u8(0x4f, 0x86, 0xd6),
                horizon: Color::srgb_u8(0xd9, 0xec, 0xf7),
                midpoint: 0.2527,
                stars: 0.0,
            },
        }
    }
}

fn linear(colour: Color) -> Vec4 {
    LinearRgba::from(colour).to_vec4()
}

/// The dome's colours for an environment. Plain is the ground colour
/// throughout, so the dome is invisible against the clear and the floor.
fn sky_for(environment: Environment, look: &Look) -> SkyUniform {
    let ground = linear(look.ground);
    match environment {
        Environment::Plain => SkyUniform {
            zenith: ground,
            horizon: ground,
            ground,
            midpoint: 0.5,
            ..default()
        },
        Environment::Sky => SkyUniform {
            zenith: linear(look.zenith),
            horizon: linear(look.horizon),
            ground,
            stars: look.stars,
            // The shader compares against the height of the view direction,
            // which is the sine of its elevation.
            midpoint: look.midpoint.sin(),
            ..default()
        },
    }
}

/// One directional light of the rig: its colour, its illuminance, and where
/// Studio places it in camera space.
struct RigLamp {
    colour: Color,
    lux: f32,
    from: Vec3,
    /// Whether it casts shadows.
    shadows: bool,
}

/// Studio's camera-relative rig: a warm key above and to the left, a cool fill
/// low and to the right, both aimed one unit ahead of the camera. Only the key
/// casts: one shadow reads as the scene's light, two read as staging.
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

#[derive(ShaderType, Clone, Debug, Default)]
pub struct SkyUniform {
    pub zenith: Vec4,
    pub horizon: Vec4,
    pub ground: Vec4,
    /// How strongly stars show, from none to full.
    pub stars: f32,
    /// The height of a unit direction — the sine of its elevation — at which
    /// the sky is halfway from the horizon colour to the zenith's.
    pub midpoint: f32,
    pub _padding: Vec2,
}

/// The sky dome's material; see `sky.wesl`.
#[derive(Asset, TypePath, AsBindGroup, Clone, Debug, Default)]
pub struct SkyMaterial {
    #[uniform(0)]
    pub sky: SkyUniform,
}

impl Material for SkyMaterial {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Path(
            AssetPath::from_path_buf(embedded_path!("sky.wesl")).with_source("embedded"),
        )
    }
}

#[derive(ShaderType, Clone, Debug, Default)]
pub struct FloorUniform {
    pub colour: Vec4,
    /// How far a full shadow darkens the floor.
    pub shadow: f32,
    pub _padding: Vec3,
}

/// The floor's material; see `floor.wesl`.
#[derive(Asset, TypePath, AsBindGroup, Clone, Debug, Default)]
pub struct FloorMaterial {
    #[uniform(0)]
    pub surface: FloorUniform,
}

impl Material for FloorMaterial {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Path(
            AssetPath::from_path_buf(embedded_path!("floor.wesl")).with_source("embedded"),
        )
    }
}

/// Marks a mesh that is part of the stage rather than the content on it, which
/// framing and the scene's census leave out.
#[derive(Component)]
pub struct StageMesh;

/// Marks the sky dome.
#[derive(Component)]
struct Dome;

/// Marks the floor.
#[derive(Component)]
struct Floor;

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
        // Carried in the binary rather than loaded from the asset root, which
        // here is wherever the robot happens to live.
        embedded_asset!(app, "sky.wesl");
        embedded_asset!(app, "floor.wesl");
        app.add_plugins((
            InfiniteGridPlugin,
            MaterialPlugin::<SkyMaterial>::default(),
            MaterialPlugin::<FloorMaterial>::default(),
        ))
        .init_resource::<Palette>()
        .add_systems(Startup, build_stage.after(crate::setup))
        .add_systems(
            Update,
            (
                stage_keys,
                apply_look.run_if(resource_changed::<Stage>.or_else(resource_changed::<Palette>)),
                (apply_lighting, show_stage).run_if(resource_changed::<Stage>),
            )
                .chain(),
        );
    }
}

/// Builds everything the stage keeps for the whole run, whichever choices are
/// current: the camera's finish, the sky dome, the floor, the grid and the
/// world rig's anchor. What the choices change is applied separately, so a
/// change at runtime takes the same path as the first frame.
fn build_stage(
    mut commands: Commands,
    opts: Res<SceneOptions>,
    cameras: Query<Entity, With<OrbitCamera>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut skies: ResMut<Assets<SkyMaterial>>,
    mut floors: ResMut<Assets<FloorMaterial>>,
) {
    let Ok(main) = cameras.single() else {
        log::error!("no scene camera to dress");
        return;
    };

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
        rig_ambient(),
    ));

    // Riding the camera, so it surrounds the camera wherever the orbit takes
    // it. Turned inside out, because it is only ever seen from within.
    let dome = Mesh::from(Sphere::new(DOME_RADIUS))
        .with_inverted_winding()
        .expect("a sphere is an indexed triangle list");
    commands.spawn((
        Mesh3d(meshes.add(dome)),
        MeshMaterial3d(skies.add(SkyMaterial::default())),
        Transform::IDENTITY,
        ChildOf(main),
        NotShadowCaster,
        NotShadowReceiver,
        StageMesh,
        Dome,
    ));

    commands.spawn((
        Mesh3d(
            meshes.add(
                Plane3d::default()
                    .mesh()
                    .size(2.0 * DOME_RADIUS, 2.0 * DOME_RADIUS),
            ),
        ),
        MeshMaterial3d(floors.add(FloorMaterial::default())),
        Transform::from_xyz(0.0, FLOOR_HEIGHT, 0.0),
        NotShadowCaster,
        StageMesh,
        Floor,
    ));

    commands.spawn((
        Transform::from_rotation(default_view(&opts)),
        Visibility::default(),
        RigAnchor,
    ));

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
            Environment::Plain => Environment::Sky,
            Environment::Sky => Environment::Plain,
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

/// Colours everything the environment and theme touch: the clear, the dome,
/// the floor and the grid.
#[allow(clippy::too_many_arguments)]
fn apply_look(
    stage: Res<Stage>,
    palette: Res<Palette>,
    mut cameras: Query<&mut Camera, With<OrbitCamera>>,
    mut skies: ResMut<Assets<SkyMaterial>>,
    domes: Query<&MeshMaterial3d<SkyMaterial>, With<Dome>>,
    mut floors: ResMut<Assets<FloorMaterial>>,
    floor: Query<&MeshMaterial3d<FloorMaterial>, With<Floor>>,
    mut grids: Query<&mut InfiniteGridSettings, With<InfiniteGrid>>,
) {
    let look = palette.look(stage.theme);
    // Behind the dome, so only ever seen at its seams; the ground colour is
    // what every seam should show.
    for mut camera in &mut cameras {
        camera.clear_color = ClearColorConfig::Custom(look.ground);
    }
    for handle in &domes {
        if let Some(mut material) = skies.get_mut(&handle.0) {
            material.sky = sky_for(stage.environment, look);
        }
    }
    for handle in &floor {
        if let Some(mut material) = floors.get_mut(&handle.0) {
            material.surface = FloorUniform {
                colour: linear(look.ground),
                shadow: FLOOR_SHADOW,
                ..default()
            };
        }
    }
    for mut settings in &mut grids {
        *settings = grid(stage.theme);
    }
}

/// Hangs the rig from the camera or from the world anchor, replacing whatever
/// hung there before.
fn apply_lighting(
    stage: Res<Stage>,
    opts: Res<SceneOptions>,
    mut commands: Commands,
    main: Query<Entity, With<OrbitCamera>>,
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

    commands.entity(parent).with_children(|hanging| {
        for lamp in rig() {
            hanging.spawn((
                DirectionalLight {
                    color: lamp.colour,
                    illuminance: lamp.lux,
                    shadow_maps_enabled: lamp.shadows && opts.shadows,
                    ..default()
                },
                aimed(lamp.from),
                RigLight,
            ));
        }
    });
}

fn show_stage(stage: Res<Stage>, mut readouts: Query<&mut Text, With<StageReadout>>) {
    for mut text in &mut readouts {
        text.0 = format!(
            "[E] environment: {:?}   [L] lighting: {:?}   [T] theme: {:?}",
            stage.environment, stage.lighting, stage.theme,
        );
    }
}

#[cfg(test)]
mod tests {
    use bevy::ecs::system::RunSystemOnce;

    use super::*;

    /// A world holding what the stage dresses — the camera, the dome, the
    /// floor and the anchor — with nothing rendering.
    fn world(stage: Stage) -> (World, Entity, Entity) {
        let mut world = World::new();
        world.insert_resource(SceneOptions::default());
        world.insert_resource(Palette::default());
        world.insert_resource(stage);
        let main = world.spawn((Camera::default(), OrbitCamera)).id();
        let anchor = world.spawn((Transform::IDENTITY, RigAnchor)).id();

        let mut skies = Assets::<SkyMaterial>::default();
        let sky = skies.add(SkyMaterial::default());
        let mut floors = Assets::<FloorMaterial>::default();
        let floor = floors.add(FloorMaterial::default());
        world.insert_resource(skies);
        world.insert_resource(floors);
        world.spawn((MeshMaterial3d(sky), Dome));
        world.spawn((MeshMaterial3d(floor), Floor));
        (world, main, anchor)
    }

    fn stage(environment: Environment, lighting: Lighting, theme: Theme) -> Stage {
        Stage {
            theme,
            environment,
            lighting,
        }
    }

    fn dome(world: &mut World) -> SkyUniform {
        let handle = world
            .query_filtered::<&MeshMaterial3d<SkyMaterial>, With<Dome>>()
            .single(world)
            .unwrap()
            .0
            .clone();
        world
            .resource::<Assets<SkyMaterial>>()
            .get(&handle)
            .unwrap()
            .sky
            .clone()
    }

    fn floor(world: &mut World) -> FloorUniform {
        let handle = world
            .query_filtered::<&MeshMaterial3d<FloorMaterial>, With<Floor>>()
            .single(world)
            .unwrap()
            .0
            .clone();
        world
            .resource::<Assets<FloorMaterial>>()
            .get(&handle)
            .unwrap()
            .surface
            .clone()
    }

    fn hung(world: &mut World) -> Vec<(Option<Entity>, DirectionalLight, Transform)> {
        let mut query = world
            .query_filtered::<(Option<&ChildOf>, &DirectionalLight, &Transform), With<RigLight>>();
        query
            .iter(world)
            .map(|(parent, light, transform)| (parent.map(ChildOf::parent), *light, *transform))
            .collect()
    }

    /// Below the horizon every view shows the same ground, whatever is above
    /// it: the clear, the dome's lower half and the floor all agree.
    #[test]
    fn every_look_shares_one_ground() {
        for environment in [Environment::Plain, Environment::Sky] {
            for theme in [Theme::Dark, Theme::Light] {
                let (mut world, main, _) = world(stage(environment, Lighting::World, theme));
                world.run_system_once(apply_look).unwrap();
                let ground = Palette::default().look(theme).ground;
                let expected = linear(ground);
                assert!(matches!(world.get::<Camera>(main).unwrap().clear_color,
                    ClearColorConfig::Custom(colour) if colour == ground));
                assert_eq!(
                    dome(&mut world).ground,
                    expected,
                    "{environment:?} {theme:?}"
                );
                assert_eq!(
                    floor(&mut world).colour,
                    expected,
                    "{environment:?} {theme:?}"
                );
            }
        }
    }

    #[test]
    fn plain_is_the_ground_all_the_way_up_and_only_night_has_stars() {
        let look = |environment, theme| {
            let (mut world, ..) = world(stage(environment, Lighting::World, theme));
            world.run_system_once(apply_look).unwrap();
            dome(&mut world)
        };
        let plain = look(Environment::Plain, Theme::Light);
        assert_eq!(plain.zenith, plain.ground);
        assert_eq!(plain.horizon, plain.ground);
        assert_eq!(plain.stars, 0.0);

        let day = look(Environment::Sky, Theme::Light);
        assert_ne!(day.zenith, day.ground);
        assert_eq!(day.stars, 0.0, "no stars by day");

        let night = look(Environment::Sky, Theme::Dark);
        assert!(night.stars > 0.0);
    }

    /// A rig left behind by a switch keeps lighting the robot, so the count is
    /// asserted after every change rather than only the new lights' presence.
    #[test]
    fn switching_anchor_moves_the_whole_rig() {
        let (mut world, main, anchor) =
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
        let back = hung(&mut world);
        assert_eq!(back.len(), 2);
        assert!(
            back.iter().all(|(parent, ..)| *parent == Some(anchor)),
            "nothing is left hanging off the camera",
        );
    }

    /// The two anchors hold the same lights: same colours, same strengths, same
    /// directions relative to what they hang from, and the key casts under
    /// both.
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
            assert_eq!(
                world_lamp.shadow_maps_enabled,
                camera_lamp.shadow_maps_enabled
            );
            assert_eq!(world_turn.rotation, camera_turn.rotation);
        }
        assert!(fixed[0].1.shadow_maps_enabled, "the key casts");
        assert!(!fixed[1].1.shadow_maps_enabled, "the fill does not");
    }
}
