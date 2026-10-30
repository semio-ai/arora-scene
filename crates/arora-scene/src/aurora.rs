//! The aurora: curtains of light high over the night sky.
//!
//! **Geometry, not backdrop.** Each curtain is a ribbon of mesh hanging in the
//! world, so it shifts against the stars as the camera moves, as something at
//! a real height does. It is an augment to the sky rather than part of it: it
//! shows over the sky environment under a dark theme, and nowhere else.
//!
//! The curtains run nearly overhead, side by side along the direction the
//! scene opens looking in, from the horizon behind to the horizon ahead. Seen
//! from the default view they recede down the line toward the hills.
//!
//! The ribbons sway mostly side to side, with a little rise and fall. That
//! motion is applied to their vertices on the CPU each frame — a few hundred
//! of them — which costs less to keep right than a vertex shader of its own
//! would. Their light comes from `aurora.wesl`, drawn additively.
//!
//! [`Aurora`] holds what is tuned by eye; the tuning panel moves it.

use std::f32::consts::TAU;

use bevy::asset::{embedded_asset, embedded_path, AssetPath, RenderAssetUsages};
use bevy::camera::visibility::NoFrustumCulling;
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::mesh::{Indices, MeshVertexBufferLayoutRef, PrimitiveTopology};
use bevy::pbr::{Material, MaterialPipeline, MaterialPipelineKey, MaterialPlugin};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;

use crate::stage::{Environment, Stage, StageMesh, Theme};
use crate::SceneOptions;

/// What is tuned by eye.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct Aurora {
    /// How bright the curtains are at their strongest.
    pub brightness: f32,
    /// How far the ribbons wave, in metres.
    pub wave: f32,
    /// How fast they wave, in cycles a second.
    pub speed: f32,
    /// How high the curtains' lower edges hang, in metres.
    pub altitude: f32,
}

impl Default for Aurora {
    fn default() -> Self {
        Self {
            brightness: 0.31,
            wave: 60.0,
            speed: 0.001,
            altitude: 400.0,
        }
    }
}

/// One curtain's place in the sky, and its colours from Studio's aurora.
struct Curtain {
    /// How far to the right of overhead it runs, in metres; negative is left.
    offset: f32,
    /// Where it starts and ends along the line it runs, in metres ahead of the
    /// camera's starting point; negative is behind.
    from: f32,
    to: f32,
    /// How much higher than [`Aurora::altitude`] its lower edge hangs, in
    /// metres.
    lift: f32,
    /// How tall it is, in metres.
    height: f32,
    /// How far its line bends to either side, in metres. A curtain seen from
    /// beneath reads by its bright lower edge, and that edge sweeps across the
    /// sky in long curves, not straight lines.
    bend: f32,
    /// How many half-turns of bend it makes along its length.
    bends: f32,
    /// Its own offset in the wave, so the curtains do not move in step.
    phase: f32,
    low: Color,
    high: Color,
    /// Its share of the brightness.
    weight: f32,
}

fn shapes() -> [Curtain; 5] {
    let green = Color::srgb_u8(0x14, 0xe8, 0x1e);
    let red = Color::srgb_u8(0xd0, 0x2a, 0x5c);
    let magenta = Color::srgb_u8(0xb0, 0x30, 0x90);
    [
        Curtain {
            offset: -1100.0,
            from: -3000.0,
            to: 4500.0,
            lift: 60.0,
            height: 750.0,
            bend: 300.0,
            bends: 1.4,
            phase: 0.0,
            low: green,
            high: red,
            weight: 0.7,
        },
        Curtain {
            offset: -520.0,
            from: -3500.0,
            to: 5000.0,
            lift: 0.0,
            height: 900.0,
            bend: 420.0,
            bends: 2.2,
            phase: 2.1,
            low: green,
            high: magenta,
            weight: 1.0,
        },
        Curtain {
            offset: 40.0,
            from: -2500.0,
            to: 4200.0,
            lift: 90.0,
            height: 680.0,
            bend: 280.0,
            bends: 1.1,
            phase: 4.0,
            low: green,
            high: red,
            weight: 0.8,
        },
        Curtain {
            offset: 600.0,
            from: -3200.0,
            to: 4800.0,
            lift: 30.0,
            height: 820.0,
            bend: 380.0,
            bends: 1.9,
            phase: 5.3,
            low: green,
            high: magenta,
            weight: 0.9,
        },
        Curtain {
            offset: 1180.0,
            from: -2800.0,
            to: 3800.0,
            lift: 140.0,
            height: 720.0,
            bend: 320.0,
            bends: 1.6,
            phase: 1.2,
            low: green,
            high: red,
            weight: 0.6,
        },
    ]
}

/// Segments along each ribbon: enough that the waving reads as a curve over
/// several kilometres.
const SEGMENTS: usize = 240;

#[derive(ShaderType, Clone, Debug, Default)]
pub struct AuroraUniform {
    pub low: Vec4,
    pub high: Vec4,
    pub brightness: f32,
    pub _padding: Vec3,
}

/// A curtain's material; see `aurora.wesl`.
#[derive(Asset, TypePath, AsBindGroup, Clone, Debug, Default)]
pub struct AuroraMaterial {
    #[uniform(0)]
    pub aurora: AuroraUniform,
}

impl Material for AuroraMaterial {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Path(
            AssetPath::from_path_buf(embedded_path!("aurora.wesl")).with_source("embedded"),
        )
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Add
    }

    /// Both faces: the camera can stand on either side of a curtain.
    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.primitive.cull_mode = None;
        Ok(())
    }
}

/// A curtain, by its index in [`shapes`].
#[derive(Component)]
struct AuroraCurtain(usize);

pub struct AuroraPlugin;

impl Plugin for AuroraPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "aurora.wesl");
        app.add_plugins(MaterialPlugin::<AuroraMaterial>::default())
            .init_resource::<Aurora>()
            .add_systems(Startup, hang_curtains)
            .add_systems(
                Update,
                (
                    show_at_night.run_if(resource_changed::<Stage>),
                    tint.run_if(resource_changed::<Aurora>),
                    wave,
                ),
            );
    }
}

/// The direction straight ahead in the default view, on the ground plane, and
/// the one to its right: the curtains run along the first and are spaced
/// along the second.
fn ahead(opts: &SceneOptions) -> (Vec3, Vec3) {
    let forward = (Quat::from_rotation_y(opts.angle) * Vec3::NEG_Z)
        .with_y(0.0)
        .normalize_or(Vec3::NEG_Z);
    (forward, forward.cross(Vec3::Y))
}

fn hang_curtains(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<AuroraMaterial>>,
) {
    for (index, _) in shapes().iter().enumerate() {
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
        );
        let columns = SEGMENTS + 1;
        let mut uvs = Vec::with_capacity(columns * 2);
        for i in 0..columns {
            let along = i as f32 / SEGMENTS as f32;
            uvs.push([along, 0.0]);
            uvs.push([along, 1.0]);
        }
        let mut indices = Vec::with_capacity(SEGMENTS * 6);
        for i in 0..SEGMENTS as u32 {
            let (low, high) = (i * 2, i * 2 + 1);
            let (next_low, next_high) = (low + 2, high + 2);
            indices.extend([low, next_low, high, high, next_low, next_high]);
        }
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; columns * 2]);
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_NORMAL,
            vec![[0.0f32, 1.0, 0.0]; columns * 2],
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
        mesh.insert_indices(Indices::U32(indices));

        commands.spawn((
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(materials.add(AuroraMaterial::default())),
            Transform::IDENTITY,
            Visibility::Hidden,
            // Its bounds are measured once, from the empty ribbon it starts as,
            // and the wave never updates them.
            NoFrustumCulling,
            NotShadowCaster,
            NotShadowReceiver,
            StageMesh,
            AuroraCurtain(index),
        ));
    }
}

/// Shows the curtains under the night sky only.
fn show_at_night(stage: Res<Stage>, mut curtains: Query<&mut Visibility, With<AuroraCurtain>>) {
    let night = stage.environment == Environment::Sky && stage.theme == Theme::Dark;
    for mut visibility in &mut curtains {
        *visibility = if night {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
    }
}

/// Gives each curtain its colours and its share of the brightness.
fn tint(
    aurora: Res<Aurora>,
    curtains: Query<(&AuroraCurtain, &MeshMaterial3d<AuroraMaterial>)>,
    mut materials: ResMut<Assets<AuroraMaterial>>,
) {
    let shapes = shapes();
    for (curtain, handle) in &curtains {
        let shape = &shapes[curtain.0];
        if let Some(mut material) = materials.get_mut(&handle.0) {
            material.aurora = AuroraUniform {
                low: LinearRgba::from(shape.low).to_vec4(),
                high: LinearRgba::from(shape.high).to_vec4(),
                brightness: aurora.brightness * shape.weight,
                ..default()
            };
        }
    }
}

/// Moves every ribbon's vertices to where the wave has them now.
///
/// The ribbon sways side to side, in two waves of different lengths along it
/// travelling in opposite directions, and its lower edge rises and falls a
/// little. The top lags the bottom, so the curtain folds rather than moving
/// as a flat sheet.
fn wave(
    time: Res<Time>,
    aurora: Res<Aurora>,
    opts: Res<SceneOptions>,
    curtains: Query<(&AuroraCurtain, &Mesh3d, &Visibility)>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let shapes = shapes();
    let (forward, right) = ahead(&opts);
    let t = time.elapsed_secs() * aurora.speed * TAU;
    for (curtain, handle, visibility) in &curtains {
        if *visibility == Visibility::Hidden {
            continue;
        }
        let shape = &shapes[curtain.0];
        let Some(mut mesh) = meshes.get_mut(&handle.0) else {
            continue;
        };
        let mut positions = Vec::with_capacity((SEGMENTS + 1) * 2);
        for i in 0..=SEGMENTS {
            let along = i as f32 / SEGMENTS as f32;
            let ahead = shape.from + (shape.to - shape.from) * along;
            let sway = |lag: f32| {
                aurora.wave * (TAU * 2.3 * along + t + shape.phase + lag).sin()
                    + 0.5 * aurora.wave * (TAU * 5.1 * along - 0.7 * t + shape.phase).sin()
            };
            let rise = 0.2 * aurora.wave * (TAU * 3.7 * along - 1.3 * t + shape.phase).sin();
            let bent = shape.offset
                + shape.bend * (std::f32::consts::PI * shape.bends * along + shape.phase).sin();
            let base = forward * ahead + Vec3::Y * (aurora.altitude + shape.lift + rise);
            let bottom = base + right * (bent + sway(0.0));
            let top = base + right * (bent + sway(0.5)) + Vec3::Y * shape.height;
            positions.push(bottom.to_array());
            positions.push(top.to_array());
        }
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    }
}
