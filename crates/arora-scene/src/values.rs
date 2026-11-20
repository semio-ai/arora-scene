//! Values pushed into the scene from outside, applied in one place.
//!
//! A host — a device stream, an inspector, a page — writes feature values
//! addressed by the ids `RobotData` gives: an animatable, which lands
//! wherever its bindings say, or an element directly. [`apply_values`] is the
//! single place they reach the world, and it requests a frame whenever it
//! applied anything, so a push is drawn the frame it arrives even when the
//! scene is otherwise idle and not redrawing. Any feature added here is
//! covered by that without a second thought.

use arora_visuals::binding::SceneBinding;
use arora_visuals::instance::EntityInstance;
use arora_visuals::joint_gizmo::JointGizmo;

use crate::joints::JointRig;
use arora_visuals::meta::FeatureKind;
use arora_visuals::value::Decoded;
use bevy::mesh::morph::MorphWeights;
use bevy::prelude::*;
use bevy::window::RequestRedraw;

/// One value for one feature.
#[derive(Clone, Debug)]
pub struct ValueWrite {
    /// The instance it is for; `None` writes every instance.
    pub instance: Option<String>,
    /// An animatable's id, or failing that an element's.
    pub target: String,
    /// The feature an element write changes. An animatable carries its own
    /// feature, so a write to one needs none, and a joint's value has no
    /// feature to name.
    pub feature: Option<FeatureKind>,
    pub value: Decoded,
    /// A time the host attached, handed back in [`AppliedValues`] once the
    /// write lands: what a host measuring push-to-drawn latency times from.
    pub stamp: Option<f64>,
}

/// Writes waiting for the next frame, in the order they were pushed.
#[derive(Resource, Default)]
pub struct ValueWrites(pub Vec<ValueWrite>);

/// What the last frame applied: what a host reads to know a push has reached
/// the world. A write that landed nowhere (an unknown id, a feature its
/// target lacks) is in neither field.
#[derive(Resource, Default)]
pub struct AppliedValues {
    /// How many places took a value.
    pub count: usize,
    /// The stamp of each write that landed somewhere.
    pub stamps: Vec<f64>,
}

pub struct ValuesPlugin;

impl Plugin for ValuesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ValueWrites>()
            .init_resource::<AppliedValues>()
            .add_systems(Update, apply_values.in_set(ApplyValues));
    }
}

/// Where values are applied; a host that feeds [`ValueWrites`] from its own
/// queue runs before it.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct ApplyValues;

/// Meshes given a material of their own, so a write addressed to one element
/// changes that element alone.
#[derive(Default)]
pub struct OwnMaterials(bevy::platform::collections::HashSet<Entity>);

/// The materials an instance's animatables drive, copied for that instance.
///
/// Copies of one GLB share its material assets, so a material write through
/// an animatable would otherwise change every copy. Each instance gets one
/// copy of each material its animatables write, the first time, shared by
/// the elements of that instance that shared it: within a robot, an
/// animatable landing on several elements still changes them together.
#[derive(Default)]
pub struct InstanceMaterials {
    /// (instance root, the material it shared) → its own copy.
    copies: bevy::platform::collections::HashMap<
        (Entity, AssetId<StandardMaterial>),
        Handle<StandardMaterial>,
    >,
    /// Every copy made, so a mesh already on one is left on it.
    made: bevy::platform::collections::HashSet<AssetId<StandardMaterial>>,
}

#[allow(clippy::too_many_arguments)]
pub fn apply_values(
    mut writes: ResMut<ValueWrites>,
    mut applied: ResMut<AppliedValues>,
    instances: Query<(Entity, &EntityInstance, Option<&JointRig>)>,
    robots: Query<&crate::robots::RobotAsset>,
    mut transforms: Query<&mut Transform>,
    mut weights: Query<&mut MorphWeights>,
    mut material_handles: Query<&mut MeshMaterial3d<StandardMaterial>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut joint_gizmos: Query<&mut JointGizmo>,
    children: Query<&Children>,
    mut own: Local<OwnMaterials>,
    mut shared: Local<InstanceMaterials>,
    mut redraw: MessageWriter<RequestRedraw>,
) {
    applied.count = 0;
    applied.stamps.clear();
    if writes.0.is_empty() {
        return;
    }
    // A write for a robot still loading has nowhere to land yet, and a host
    // may well write as soon as it asks for the robot: keep it for the frame
    // the robot arrives in, rather than dropping it. A write for every
    // instance waits while there are none. A write for an instance nothing is
    // loading is dropped, as nothing will ever take it.
    let indexed = |id: &str| instances.iter().any(|(_, instance, _)| instance.id == id);
    let coming = |id: &str| robots.iter().any(|robot| robot.instance == id);
    let mut later = Vec::new();
    let mut now = Vec::new();
    for write in writes.0.drain(..) {
        match &write.instance {
            Some(id) if indexed(id) => now.push(write),
            Some(id) if coming(id) => later.push(write),
            Some(id) => log::debug!("no robot {id:?} for a write to {:?}", write.target),
            None if instances.is_empty() => later.push(write),
            None => now.push(write),
        }
    }
    writes.0 = later;
    let mut count = 0;
    for write in now {
        let before = count;
        for (root, instance, rig) in &instances {
            if write.instance.as_ref().is_some_and(|id| *id != instance.id) {
                continue;
            }
            // A joint's value poses the bodies it moves, and any joint gizmo
            // on them shows it, so a control and the robot never disagree.
            if let Some(joints) = rig.and_then(|rig| rig.0.get(&write.target)) {
                let value = match write.value {
                    Decoded::Scalar(s) => s,
                    Decoded::Triple(v) => v[0],
                };
                for joint in joints {
                    let at = joint.joint_value(value);
                    if let Ok(mut transform) = transforms.get_mut(joint.body) {
                        *transform = joint.pose(at);
                        count += 1;
                    }
                    for mut gizmo in &mut joint_gizmos {
                        if gizmo.target == joint.body && gizmo.value != at {
                            gizmo.value = at;
                        }
                    }
                }
                continue;
            }
            let bindings = &instance.bindings;
            let landing: Vec<SceneBinding> = match bindings.by_id.get(&write.target) {
                // An animatable lands on every element it drives, sharing a
                // material where they share one: that is what it declares.
                // Shared with this robot's elements only, not every copy of
                // its GLB.
                Some(found) => {
                    for binding in found.iter().filter(|b| is_material(&b.feature)) {
                        let Ok(mut handle) = material_handles.get_mut(binding.entity) else {
                            continue;
                        };
                        if shared.made.contains(&handle.0.id()) {
                            continue;
                        }
                        let key = (root, handle.0.id());
                        let copy = match shared.copies.get(&key) {
                            Some(copy) => copy.clone(),
                            None => {
                                let Some(material) = materials.get(&handle.0).cloned() else {
                                    continue;
                                };
                                let copy = materials.add(material);
                                shared.made.insert(copy.id());
                                shared.copies.insert(key, copy.clone());
                                copy
                            }
                        };
                        handle.0 = copy;
                    }
                    found.clone()
                }
                None => match bindings.elements.get(&write.target) {
                    Some(element) => {
                        let Some(feature) = &write.feature else {
                            continue;
                        };
                        let entities = if is_material(feature) {
                            let mut meshes = primitives(element.node, &children, |e| {
                                material_handles.contains(e)
                            });
                            if meshes.is_empty() {
                                meshes.extend(element.mesh);
                            }
                            // An element written directly is one element,
                            // but the GLB may share its materials with
                            // others, which a material write would change
                            // too. Each mesh gets a copy of its own the first
                            // time.
                            for &mesh in &meshes {
                                if own.0.insert(mesh) {
                                    if let Ok(mut handle) = material_handles.get_mut(mesh) {
                                        if let Some(copy) = materials.get(&handle.0).cloned() {
                                            handle.0 = materials.add(copy);
                                        }
                                    }
                                }
                            }
                            meshes
                        } else {
                            vec![element.node]
                        };
                        entities
                            .into_iter()
                            .map(|entity| SceneBinding {
                                entity,
                                feature: feature.clone(),
                                axis: None,
                                morph_index: None,
                            })
                            .collect()
                    }
                    None => continue,
                },
            };
            for binding in landing {
                if apply(
                    &binding,
                    write.value,
                    &mut transforms,
                    &mut weights,
                    &material_handles,
                    &mut materials,
                ) {
                    count += 1;
                }
            }
        }
        if count > before {
            applied.stamps.extend(write.stamp);
        }
    }
    applied.count = count;
    if count > 0 {
        redraw.write(RequestRedraw);
    }
}

/// The meshes a glTF node draws with: the node itself if it carries a mesh,
/// otherwise its direct children that do, which is where a node with several
/// primitives keeps one mesh, and one material, per primitive. Only direct
/// children, because a deeper mesh belongs to a child node, which is another
/// element.
pub fn primitives(
    node: Entity,
    children: &Query<&Children>,
    is_mesh: impl Fn(Entity) -> bool,
) -> Vec<Entity> {
    if is_mesh(node) {
        return vec![node];
    }
    children
        .get(node)
        .map(|c| c.iter().filter(|&e| is_mesh(e)).collect())
        .unwrap_or_default()
}

/// Whether a feature is written onto a material rather than a transform or
/// morph weights.
fn is_material(feature: &FeatureKind) -> bool {
    !matches!(
        feature,
        FeatureKind::Translation
            | FeatureKind::Rotation
            | FeatureKind::Scale
            | FeatureKind::Morph(_)
    )
}

/// One value onto one place; whether it landed.
fn apply(
    binding: &SceneBinding,
    value: Decoded,
    transforms: &mut Query<&mut Transform>,
    weights: &mut Query<&mut MorphWeights>,
    material_handles: &Query<&mut MeshMaterial3d<StandardMaterial>>,
    materials: &mut Assets<StandardMaterial>,
) -> bool {
    let triple = |current: [f32; 3]| match value {
        Decoded::Triple(v) => match binding.axis {
            Some(axis) => {
                let mut out = current;
                out[axis] = v[axis];
                out
            }
            None => v,
        },
        Decoded::Scalar(s) => match binding.axis {
            Some(axis) => {
                let mut out = current;
                out[axis] = s;
                out
            }
            None => [s; 3],
        },
    };
    let scalar = match value {
        Decoded::Scalar(s) => s,
        Decoded::Triple(v) => v[0],
    };
    match &binding.feature {
        FeatureKind::Translation | FeatureKind::Rotation | FeatureKind::Scale => {
            let Ok(mut transform) = transforms.get_mut(binding.entity) else {
                return false;
            };
            match &binding.feature {
                FeatureKind::Translation => {
                    transform.translation = Vec3::from(triple(transform.translation.to_array()));
                }
                FeatureKind::Rotation => {
                    // An euler in the web's ZYX order, as `FeatureKind` says.
                    let (z, y, x) = transform.rotation.to_euler(EulerRot::ZYX);
                    let [x, y, z] = triple([x, y, z]);
                    transform.rotation = Quat::from_euler(EulerRot::ZYX, z, y, x);
                }
                _ => transform.scale = Vec3::from(triple(transform.scale.to_array())),
            }
            true
        }
        FeatureKind::Morph(_) => {
            let (Some(index), Ok(mut weights)) =
                (binding.morph_index, weights.get_mut(binding.entity))
            else {
                return false;
            };
            match weights.weights_mut().get_mut(index) {
                Some(weight) => {
                    *weight = scalar;
                    true
                }
                None => false,
            }
        }
        FeatureKind::Color | FeatureKind::Opacity | FeatureKind::Emissive => {
            let Some(mut material) = material_handles
                .get(binding.entity)
                .ok()
                .and_then(|handle| materials.get_mut(&handle.0))
            else {
                return false;
            };
            match &binding.feature {
                FeatureKind::Color => {
                    let current = material.base_color.to_linear();
                    let [r, g, b] = triple([current.red, current.green, current.blue]);
                    material.base_color = Color::linear_rgba(r, g, b, current.alpha);
                }
                FeatureKind::Opacity => {
                    material.base_color.set_alpha(scalar);
                    material.alpha_mode = if scalar < 1.0 {
                        AlphaMode::Blend
                    } else {
                        AlphaMode::Opaque
                    };
                }
                _ => {
                    let current = material.emissive;
                    let [r, g, b] = triple([current.red, current.green, current.blue]);
                    material.emissive = LinearRgba::new(r, g, b, current.alpha);
                }
            }
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use arora_visuals::binding::SceneBindings;

    use super::*;
    use crate::joints::{JointBinding, JointRig};
    use crate::robot::{Joint, JointKind};
    use crate::robots::{Incoming, RobotAsset};

    const TILT: &str = "tilt-value";

    fn tilt() -> Joint {
        Joint {
            name: "tilt".into(),
            kind: JointKind::Revolute,
            axis: Vec3::X,
            child_node: 0,
            child: "head".into(),
            animatable: Some(TILT.into()),
            mimic: None,
            min: -1.0,
            max: 1.0,
            default: 0.0,
            color: Srgba::WHITE,
        }
    }

    fn asset(instance: &str) -> RobotAsset {
        RobotAsset {
            instance: instance.into(),
            key: None,
            glb: String::new(),
            meta: Default::default(),
            joints: vec![tilt()],
        }
    }

    /// A robot showing in `instance`, indexed, whose tilt joint moves the
    /// body it returns.
    fn showing(app: &mut App, instance: &str) -> Entity {
        let world = app.world_mut();
        let body = world.spawn(Transform::default()).id();
        let mut rig = JointRig::default();
        rig.0.insert(
            TILT.into(),
            vec![JointBinding::new(&tilt(), body, Transform::default()).unwrap()],
        );
        world.spawn((
            rig,
            asset(instance),
            EntityInstance {
                id: instance.into(),
                asset_path: String::new(),
                meta: Default::default(),
                bindings: SceneBindings::default(),
                camera: Entity::PLACEHOLDER,
                draw_order: 0,
                ready: true,
            },
        ));
        body
    }

    fn write(app: &mut App, instance: &str, value: f32) {
        app.world_mut()
            .resource_mut::<ValueWrites>()
            .0
            .push(ValueWrite {
                instance: Some(instance.into()),
                target: TILT.into(),
                feature: None,
                value: Decoded::Scalar(value),
                stamp: None,
            });
    }

    fn turned(app: &App, body: Entity) -> f32 {
        app.world()
            .get::<Transform>(body)
            .unwrap()
            .rotation
            .to_axis_angle()
            .1
    }

    /// A material write through an animatable changes its own robot's
    /// material, not the one its GLB shares with another copy, and the
    /// elements of one robot that share it still change together.
    #[test]
    fn a_material_write_changes_its_own_copy_of_a_glb_only() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<StandardMaterial>()
            .add_message::<RequestRedraw>()
            .add_plugins(ValuesPlugin);
        let material = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial {
                base_color: Color::BLACK,
                ..default()
            });
        let mut robot = |instance: &str| {
            let world = app.world_mut();
            let meshes: Vec<Entity> = (0..2)
                .map(|_| world.spawn(MeshMaterial3d(material.clone())).id())
                .collect();
            let mut bindings = SceneBindings::default();
            bindings.by_id.insert(
                "colour".into(),
                meshes
                    .iter()
                    .map(|&entity| SceneBinding {
                        entity,
                        feature: FeatureKind::Color,
                        axis: None,
                        morph_index: None,
                    })
                    .collect(),
            );
            bindings.indexed = true;
            world.spawn((
                asset(instance),
                EntityInstance {
                    id: instance.into(),
                    asset_path: String::new(),
                    meta: Default::default(),
                    bindings,
                    camera: Entity::PLACEHOLDER,
                    draw_order: 0,
                    ready: true,
                },
            ));
            meshes
        };
        let a = robot("a");
        let b = robot("b");
        app.world_mut()
            .resource_mut::<ValueWrites>()
            .0
            .push(ValueWrite {
                instance: Some("a".into()),
                target: "colour".into(),
                feature: None,
                value: Decoded::Triple([1.0, 0.0, 0.0]),
                stamp: None,
            });
        app.update();

        let colour = |mesh: Entity| {
            let world = app.world();
            let handle = &world
                .get::<MeshMaterial3d<StandardMaterial>>(mesh)
                .unwrap()
                .0;
            let material = world
                .resource::<Assets<StandardMaterial>>()
                .get(handle)
                .unwrap();
            (handle.id(), material.base_color.to_linear().red)
        };
        let (a0, red_a0) = colour(a[0]);
        let (a1, red_a1) = colour(a[1]);
        let (b0, red_b0) = colour(b[0]);
        assert_eq!(a0, a1, "a robot's elements still share their copy");
        assert_ne!(a0, b0, "each robot has its own");
        assert!((red_a0 - 1.0).abs() < 1e-5 && (red_a1 - 1.0).abs() < 1e-5);
        assert!((red_b0 - 1.0).abs() > 1e-3, "b was not written");
    }

    /// A write lands on the robot it names and no other; one for a robot
    /// still loading waits for it; one for an instance nothing is loading is
    /// dropped.
    #[test]
    fn a_write_reaches_the_instance_it_names() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<StandardMaterial>()
            .add_message::<RequestRedraw>()
            .add_plugins(ValuesPlugin);
        let a = showing(&mut app, "a");
        let b = showing(&mut app, "b");
        app.world_mut().spawn((asset("c"), Incoming));

        write(&mut app, "a", 0.5);
        write(&mut app, "c", 0.25);
        write(&mut app, "nobody", 0.75);
        app.update();

        assert!((turned(&app, a) - 0.5).abs() < 1e-5);
        assert_eq!(turned(&app, b), 0.0, "b was not written");
        let waiting: Vec<_> = app
            .world()
            .resource::<ValueWrites>()
            .0
            .iter()
            .map(|write| write.instance.clone())
            .collect();
        assert_eq!(waiting, vec![Some("c".to_string())]);
    }
}
