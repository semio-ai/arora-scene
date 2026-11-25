//! Values pushed into the scene from outside, applied in one place.
//!
//! A host — a device stream, an inspector, a page — writes feature values
//! addressed by the ids `RobotData` gives: an animatable, which lands
//! wherever its bindings say, or an element directly. [`apply_values`] is the
//! single place they reach the world, and it requests a frame whenever it
//! applied anything, so a push is drawn the frame it arrives even when the
//! scene is otherwise idle and not redrawing. Any feature added here is
//! covered by that without a second thought.

use std::collections::HashMap;

use arora_visuals::binding::SceneBinding;
use arora_visuals::ghost::{HasSolidMaterial, SolidMaterial};
use arora_visuals::instance::{EntityInstance, InstanceIndex, InstanceSystems};
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
    /// The feature an element write changes, as `RobotData` names it:
    /// `translation`, `color`, a per-axis `translation.x`, one of the
    /// element's morph target names. Resolved against the element it lands
    /// on, since only the element knows its morph targets. An animatable
    /// carries its own feature, so a write to one needs none, and a joint's
    /// value has no feature to name.
    pub feature: Option<String>,
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
            .init_resource::<Emission>()
            .add_plugins(arora_visuals::instance::InstancePlugin)
            .configure_sets(Update, ApplyValues.in_set(InstanceSystems::Apply))
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

/// What a material emits before its intensity scales it, and that intensity,
/// for each material an emissive or intensity write has touched.
///
/// A material holds only the product, so writing the intensity onto it needs
/// the colour kept beside it: the colour is the material's emissive when it is
/// first touched, which is what its GLB gave it at that intensity, 1. Kept by
/// material, as the material is what the two writes share, and dropped once
/// the material is. A resource rather than the value system's own, because
/// whatever else copies a material has to carry its state to the copy.
#[derive(Resource, Default)]
pub struct Emission(HashMap<AssetId<StandardMaterial>, Emitting>);

#[derive(Clone, Copy, Debug, PartialEq)]
struct Emitting {
    colour: [f32; 3],
    intensity: f32,
}

impl Emission {
    /// The material's state, taken from what it emits now the first time.
    fn of(&mut self, id: AssetId<StandardMaterial>, material: &StandardMaterial) -> &mut Emitting {
        self.0.entry(id).or_insert_with(|| Emitting {
            colour: [
                material.emissive.red,
                material.emissive.green,
                material.emissive.blue,
            ],
            intensity: 1.0,
        })
    }

    /// A copy of `from` emits as it does, so its state goes with it: read
    /// afresh, the copy's emissive would be taken as the colour with the
    /// intensity already in it.
    /// A material's colour and intensity, given one.
    #[cfg(test)]
    pub(crate) fn set(&mut self, id: AssetId<StandardMaterial>, colour: [f32; 3], intensity: f32) {
        self.0.insert(id, Emitting { colour, intensity });
    }

    /// A material's colour and intensity, if it has been touched.
    #[cfg(test)]
    pub(crate) fn state(&self, id: AssetId<StandardMaterial>) -> Option<([f32; 3], f32)> {
        self.0.get(&id).map(|e| (e.colour, e.intensity))
    }

    pub fn copied(&mut self, from: AssetId<StandardMaterial>, to: AssetId<StandardMaterial>) {
        if let Some(state) = self.0.get(&from).copied() {
            self.0.insert(to, state);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn apply_values(
    mut writes: ResMut<ValueWrites>,
    mut applied: ResMut<AppliedValues>,
    instances: Query<(Entity, &EntityInstance, Option<&JointRig>)>,
    mut transforms: Query<&mut Transform>,
    mut weights: Query<&mut MorphWeights>,
    // A mesh drawn as a ghost keeps its own material aside; a write reaches
    // that one, which the ghost follows and the mesh shows once solid.
    mut material_handles: Query<SolidMaterial, HasSolidMaterial>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut joint_gizmos: Query<&mut JointGizmo>,
    children: Query<&Children>,
    mut own: Local<OwnMaterials>,
    mut shared: Local<InstanceMaterials>,
    mut emission: ResMut<Emission>,
    mut redraw: MessageWriter<RequestRedraw>,
    index: Res<InstanceIndex>,
) {
    applied.count = 0;
    applied.stamps.clear();
    // A robot unloaded or swapped out takes its copies with it: holding their
    // handles would keep the materials, and what they reference, alive for
    // the life of the page.
    if !shared.copies.is_empty() {
        let InstanceMaterials { copies, made } = &mut *shared;
        copies.retain(|(root, _), copy| {
            let live = instances.contains(*root);
            if !live {
                made.remove(&copy.id());
            }
            live
        });
    }
    if !own.0.is_empty() {
        own.0.retain(|mesh| material_handles.contains(*mesh));
    }
    if !emission.0.is_empty() {
        emission.0.retain(|id, _| materials.contains(*id));
    }
    if writes.0.is_empty() {
        return;
    }
    // A write for a robot still loading has nowhere to land yet, and a host
    // may well write as soon as it asks for the robot: keep it for the frame
    // the robot arrives in, rather than dropping it. A write for every
    // instance waits while there are none. A write for an instance nothing is
    // loading is dropped, as nothing will ever take it — a robot that loaded
    // but will never be indexed included, as it is no longer loading.
    let indexed = |id: &str| index.get(id).is_some();
    let coming = |id: &str| index.is_loading(id);
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
        let on: Vec<_> = match &write.instance {
            Some(id) => instances.iter_many(index.all(id)).flatten().collect(),
            None => instances.iter().collect(),
        };
        for (root, instance, rig) in on {
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
                        let Some(current) = handle.handle().cloned() else {
                            continue;
                        };
                        if shared.made.contains(&current.id()) {
                            continue;
                        }
                        let key = (root, current.id());
                        let copy = match shared.copies.get(&key) {
                            Some(copy) => copy.clone(),
                            None => {
                                let Some(material) = materials.get(&current).cloned() else {
                                    continue;
                                };
                                let copy = materials.add(material);
                                emission.copied(current.id(), copy.id());
                                shared.made.insert(copy.id());
                                shared.copies.insert(key, copy.clone());
                                copy
                            }
                        };
                        handle.set(copy);
                    }
                    found.clone()
                }
                None => match bindings.elements.get(&write.target) {
                    Some(element) => {
                        let Some(name) = &write.feature else {
                            continue;
                        };
                        let Some((feature, axis, morph_index)) =
                            element_feature(instance, &write.target, name, write.value)
                        else {
                            continue;
                        };
                        let feature = &feature;
                        if let FeatureKind::Morph(_) = feature {
                            let Some(bearer) = element.morph else {
                                continue;
                            };
                            vec![SceneBinding {
                                entity: bearer,
                                feature: feature.clone(),
                                axis: None,
                                morph_index,
                            }]
                        } else {
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
                                            let Some(current) = handle.handle().cloned() else {
                                                continue;
                                            };
                                            if let Some(copy) = materials.get(&current).cloned() {
                                                let copy = materials.add(copy);
                                                emission.copied(current.id(), copy.id());
                                                handle.set(copy);
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
                                    axis,
                                    morph_index: None,
                                })
                                .collect()
                        }
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
                    &mut emission,
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

/// The feature an element write names, resolved against the element: its
/// kind, the component a per-axis name drives, and a morph target's index.
/// `None`, said in the log, for a name the element does not have or a value
/// of the wrong shape for it: one number for a per-axis name, a morph, or a
/// scalar feature, three for a vector or a colour, and either for `scale`,
/// where one scales evenly.
fn element_feature(
    instance: &EntityInstance,
    element_id: &str,
    name: &str,
    value: Decoded,
) -> Option<(FeatureKind, Option<usize>, Option<usize>)> {
    let morphs = instance
        .meta
        .elements
        .iter()
        .find(|element| element.id == element_id)
        .map_or(&[][..], |element| element.morph_targets.as_slice());
    let Some((kind, axis)) = FeatureKind::parse(name, morphs) else {
        log::warn!("{}: {element_id} has no feature {name:?}", instance.id);
        return None;
    };
    let scalar = matches!(value, Decoded::Scalar(_));
    let fits = match (&kind, axis) {
        (_, Some(_)) => scalar,
        (FeatureKind::Scale, None) => true,
        (
            FeatureKind::Translation
            | FeatureKind::Rotation
            | FeatureKind::Color
            | FeatureKind::Emissive
            | FeatureKind::Specular,
            None,
        ) => !scalar,
        _ => scalar,
    };
    if !fits {
        log::warn!(
            "{}: {name} of {element_id} does not take {value:?}",
            instance.id
        );
        return None;
    }
    let morph_index = match &kind {
        FeatureKind::Morph(target) => morphs.iter().position(|m| m == target),
        _ => None,
    };
    Some((kind, axis, morph_index))
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
    material_handles: &Query<SolidMaterial, HasSolidMaterial>,
    materials: &mut Assets<StandardMaterial>,
    emission: &mut Emission,
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
        FeatureKind::Color
        | FeatureKind::Opacity
        | FeatureKind::Emissive
        | FeatureKind::EmissiveIntensity
        | FeatureKind::Metalness => {
            let Some((id, mut material)) = material_handles
                .get(binding.entity)
                .ok()
                .and_then(|solid| solid.handle().cloned())
                .and_then(|handle| Some((handle.id(), materials.get_mut(&handle)?)))
            else {
                return false;
            };
            match &binding.feature {
                FeatureKind::Color => {
                    let current = material.base_color.to_linear();
                    let [r, g, b] = triple([current.red, current.green, current.blue]);
                    material.base_color = Color::linear_rgba(r, g, b, current.alpha);
                }
                FeatureKind::Metalness => material.metallic = scalar,
                FeatureKind::Opacity => {
                    material.base_color.set_alpha(scalar);
                    material.alpha_mode = if scalar < 1.0 {
                        AlphaMode::Blend
                    } else {
                        AlphaMode::Opaque
                    };
                }
                _ => {
                    let state = emission.of(id, &material);
                    if binding.feature == FeatureKind::EmissiveIntensity {
                        state.intensity = scalar;
                    } else {
                        state.colour = triple(state.colour);
                    }
                    let Emitting { colour, intensity } = *state;
                    let [r, g, b] = colour.map(|channel| channel * intensity);
                    material.emissive = LinearRgba::new(r, g, b, material.emissive.alpha);
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

    /// Colour and opacity written to a ghosted robot land on its own
    /// material, which its ghost follows and it shows again once solid, and
    /// leave a robot sharing its GLB's material alone.
    #[test]
    fn a_write_to_a_ghosted_robot_reaches_its_ghost() {
        use arora_visuals::ghost::{Ghost, GhostMaterial, GhostPlugin};
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<StandardMaterial>()
            .init_asset::<bevy::shader::Shader>()
            .add_message::<RequestRedraw>()
            .add_plugins((ValuesPlugin, GhostPlugin));
        let material = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial {
                base_color: Color::BLACK,
                ..default()
            });
        let mut robot = |instance: &str| {
            let world = app.world_mut();
            let root = world.spawn_empty().id();
            let mesh = world
                .spawn((MeshMaterial3d(material.clone()), ChildOf(root)))
                .id();
            let mut bindings = SceneBindings::default();
            for (id, feature) in [
                ("colour", FeatureKind::Color),
                ("opacity", FeatureKind::Opacity),
            ] {
                bindings.by_id.insert(
                    id.into(),
                    vec![SceneBinding {
                        entity: mesh,
                        feature,
                        axis: None,
                        morph_index: None,
                    }],
                );
            }
            bindings.indexed = true;
            world.entity_mut(root).insert((
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
            (root, mesh)
        };
        let (ghosted, mesh) = robot("a");
        let (_, other) = robot("b");
        app.world_mut().entity_mut(ghosted).insert(Ghost);
        app.update();

        let write = |app: &mut App, target: &str, value: Decoded| {
            app.world_mut()
                .resource_mut::<ValueWrites>()
                .0
                .push(ValueWrite {
                    instance: Some("a".into()),
                    target: target.into(),
                    feature: None,
                    value,
                    stamp: None,
                });
            app.update();
            app.world().resource::<AppliedValues>().count
        };
        assert_eq!(
            write(&mut app, "colour", Decoded::Triple([1.0, 0.0, 0.0])),
            1
        );
        assert_eq!(write(&mut app, "opacity", Decoded::Scalar(0.25)), 1);
        app.update();
        let ghost = &app
            .world()
            .get::<MeshMaterial3d<GhostMaterial>>(mesh)
            .expect("still a ghost")
            .0;
        assert_eq!(
            app.world()
                .resource::<Assets<GhostMaterial>>()
                .get(ghost)
                .unwrap()
                .ghost
                .colour,
            Vec4::new(1.0, 0.0, 0.0, 0.25),
            "the ghost shows what was written"
        );
        let other_colour = |app: &App| {
            let handle = &app
                .world()
                .get::<MeshMaterial3d<StandardMaterial>>(other)
                .unwrap()
                .0;
            app.world()
                .resource::<Assets<StandardMaterial>>()
                .get(handle)
                .unwrap()
                .base_color
        };
        assert_eq!(other_colour(&app), Color::BLACK, "b is left alone");

        app.world_mut().entity_mut(ghosted).remove::<Ghost>();
        app.update();
        let handle = &app
            .world()
            .get::<MeshMaterial3d<StandardMaterial>>(mesh)
            .expect("solid again")
            .0;
        assert_eq!(
            app.world()
                .resource::<Assets<StandardMaterial>>()
                .get(handle)
                .unwrap()
                .base_color
                .to_linear(),
            LinearRgba::new(1.0, 0.0, 0.0, 0.25),
            "solid again, it shows what was written while it was a ghost"
        );
    }

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

    /// An element written directly gets a material of its own, so another
    /// mesh sharing its GLB material is left alone — solid, and ghosted.
    #[test]
    fn an_element_write_leaves_a_sharer_of_its_material_alone() {
        for ghosted in [false, true] {
            use arora_visuals::ghost::{Ghost, GhostOf, GhostPlugin};
            let mut app = App::new();
            app.add_plugins((MinimalPlugins, AssetPlugin::default()))
                .init_asset::<StandardMaterial>()
                .init_asset::<bevy::shader::Shader>()
                .add_message::<RequestRedraw>()
                .add_plugins((ValuesPlugin, GhostPlugin));
            let material = app
                .world_mut()
                .resource_mut::<Assets<StandardMaterial>>()
                .add(StandardMaterial {
                    base_color: Color::BLACK,
                    ..default()
                });
            let world = app.world_mut();
            let root = world.spawn_empty().id();
            let node = world.spawn(ChildOf(root)).id();
            let mesh = world
                .spawn((MeshMaterial3d(material.clone()), ChildOf(node)))
                .id();
            let sharer = world
                .spawn((MeshMaterial3d(material.clone()), ChildOf(root)))
                .id();
            let mut bindings = SceneBindings::default();
            bindings.elements.insert(
                "lid".into(),
                arora_visuals::binding::ElementTargets {
                    node,
                    mesh: Some(mesh),
                    morph: None,
                },
            );
            bindings.indexed = true;
            let mut meta = arora_visuals::meta::AssetMeta::default();
            meta.elements.push(arora_visuals::meta::Element {
                id: "lid".into(),
                name: String::new(),
                node_name: "lid".into(),
                kind: "shape".into(),
                material: None,
                morph_targets: Vec::new(),
                primitive: None,
            });
            world.entity_mut(root).insert((
                asset("a"),
                EntityInstance {
                    id: "a".into(),
                    asset_path: String::new(),
                    meta,
                    bindings,
                    camera: Entity::PLACEHOLDER,
                    draw_order: 0,
                    ready: true,
                },
            ));
            if ghosted {
                app.world_mut().entity_mut(root).insert(Ghost);
            }
            app.update();
            app.world_mut()
                .resource_mut::<ValueWrites>()
                .0
                .push(ValueWrite {
                    instance: Some("a".into()),
                    target: "lid".into(),
                    feature: Some("color".into()),
                    value: Decoded::Triple([1.0, 0.0, 0.0]),
                    stamp: None,
                });
            app.update();
            assert_eq!(
                app.world().resource::<AppliedValues>().count,
                1,
                "ghosted: {ghosted}"
            );
            let red = |app: &App, entity: Entity| {
                let world = app.world();
                let handle = world
                    .get::<MeshMaterial3d<StandardMaterial>>(entity)
                    .map(|m| m.0.clone())
                    .or_else(|| world.get::<GhostOf>(entity).map(|g| g.original.clone()))
                    .unwrap();
                world
                    .resource::<Assets<StandardMaterial>>()
                    .get(&handle)
                    .unwrap()
                    .base_color
                    .to_linear()
                    .red
            };
            assert_eq!(red(&app, mesh), 1.0, "ghosted: {ghosted}");
            assert_eq!(red(&app, sharer), 0.0, "the sharer, ghosted: {ghosted}");
        }
    }

    /// An element write names its feature as `RobotData` does, resolved
    /// against the element: a per-axis name moves one component, a morph
    /// target's name its weight, `metalness` the material; a value of the
    /// wrong shape, or a feature the element lacks, changes nothing.
    #[test]
    fn an_element_write_takes_the_names_robotdata_gives() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<StandardMaterial>()
            .add_message::<RequestRedraw>()
            .add_plugins(ValuesPlugin);
        let material = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());
        let world = app.world_mut();
        let node = world.spawn(Transform::from_xyz(1.0, 2.0, 3.0)).id();
        let mesh = world.spawn(MeshMaterial3d(material)).id();
        let bearer = world
            .spawn(MorphWeights::new(vec![0.0, 0.0], None).unwrap())
            .id();
        let mut bindings = SceneBindings::default();
        bindings.elements.insert(
            "lid".into(),
            arora_visuals::binding::ElementTargets {
                node,
                mesh: Some(mesh),
                morph: Some(bearer),
            },
        );
        bindings.indexed = true;
        let mut meta = arora_visuals::meta::AssetMeta::default();
        meta.elements.push(arora_visuals::meta::Element {
            id: "lid".into(),
            name: String::new(),
            node_name: "lid".into(),
            kind: "shape".into(),
            material: None,
            morph_targets: vec!["open".into(), "blink".into()],
            primitive: None,
        });
        world.spawn((
            asset("a"),
            EntityInstance {
                id: "a".into(),
                asset_path: String::new(),
                meta,
                bindings,
                camera: Entity::PLACEHOLDER,
                draw_order: 0,
                ready: true,
            },
        ));
        let write = |app: &mut App, feature: &str, value: Decoded| {
            app.world_mut()
                .resource_mut::<ValueWrites>()
                .0
                .push(ValueWrite {
                    instance: Some("a".into()),
                    target: "lid".into(),
                    feature: Some(feature.into()),
                    value,
                    stamp: None,
                });
            app.update();
            app.world().resource::<AppliedValues>().count
        };

        assert_eq!(write(&mut app, "translation.y", Decoded::Scalar(5.0)), 1);
        assert_eq!(
            app.world().get::<Transform>(node).unwrap().translation,
            Vec3::new(1.0, 5.0, 3.0)
        );
        assert_eq!(write(&mut app, "blink", Decoded::Scalar(0.5)), 1);
        assert_eq!(
            app.world().get::<MorphWeights>(bearer).unwrap().weights(),
            &[0.0, 0.5]
        );
        assert_eq!(write(&mut app, "metalness", Decoded::Scalar(0.25)), 1);
        let handle = app
            .world()
            .get::<MeshMaterial3d<StandardMaterial>>(mesh)
            .unwrap()
            .0
            .clone();
        let metallic = app
            .world()
            .resource::<Assets<StandardMaterial>>()
            .get(&handle)
            .unwrap()
            .metallic;
        assert_eq!(metallic, 0.25);

        // The wrong shape, and a name the element does not have.
        assert_eq!(write(&mut app, "opacity", Decoded::Triple([1.0; 3])), 0);
        assert_eq!(write(&mut app, "translation", Decoded::Scalar(9.0)), 0);
        assert_eq!(
            write(&mut app, "translation.y", Decoded::Triple([9.0; 3])),
            0
        );
        assert_eq!(write(&mut app, "wink", Decoded::Scalar(1.0)), 0);
        assert_eq!(
            app.world().get::<Transform>(node).unwrap().translation,
            Vec3::new(1.0, 5.0, 3.0)
        );
        // Scale takes one number, scaling evenly.
        assert_eq!(write(&mut app, "scale", Decoded::Scalar(2.0)), 1);
        assert_eq!(
            app.world().get::<Transform>(node).unwrap().scale,
            Vec3::splat(2.0)
        );
    }

    /// `emissiveIntensity` scales what the material emits, from the colour
    /// its GLB gave it, without losing that colour: the intensity and the
    /// colour are each written on their own and the material emits their
    /// product, so a colour written after an intensity is still scaled by it
    /// and an intensity of zero, raised again, brings the colour back. That
    /// holds across the copy an element write makes of a material an
    /// animatable has already scaled.
    #[test]
    fn emissive_intensity_scales_the_emissive_colour() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<StandardMaterial>()
            .add_message::<RequestRedraw>()
            .add_plugins(ValuesPlugin);
        let material = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial {
                emissive: LinearRgba::rgb(0.5, 0.25, 0.0),
                ..default()
            });
        let world = app.world_mut();
        let node = world.spawn(Transform::default()).id();
        let mesh = world.spawn(MeshMaterial3d(material)).id();
        let mut bindings = SceneBindings::default();
        bindings.elements.insert(
            "eye".into(),
            arora_visuals::binding::ElementTargets {
                node,
                mesh: Some(mesh),
                morph: None,
            },
        );
        bindings.by_id.insert(
            "glow".into(),
            vec![SceneBinding {
                entity: mesh,
                feature: FeatureKind::EmissiveIntensity,
                axis: None,
                morph_index: None,
            }],
        );
        bindings.indexed = true;
        let mut meta = arora_visuals::meta::AssetMeta::default();
        meta.elements.push(arora_visuals::meta::Element {
            id: "eye".into(),
            name: String::new(),
            node_name: "eye".into(),
            kind: "shape".into(),
            material: None,
            morph_targets: Vec::new(),
            primitive: None,
        });
        world.spawn((
            asset("a"),
            EntityInstance {
                id: "a".into(),
                asset_path: String::new(),
                meta,
                bindings,
                camera: Entity::PLACEHOLDER,
                draw_order: 0,
                ready: true,
            },
        ));
        let write = |app: &mut App, feature: &str, value: Decoded| {
            // The animatable when named, else the element's feature.
            let (target, feature) = match feature {
                "glow" => ("glow", None),
                feature => ("eye", Some(feature.to_string())),
            };
            app.world_mut()
                .resource_mut::<ValueWrites>()
                .0
                .push(ValueWrite {
                    instance: Some("a".into()),
                    target: target.into(),
                    feature,
                    value,
                    stamp: None,
                });
            app.update();
            assert_eq!(app.world().resource::<AppliedValues>().count, 1, "{target}");
            let handle = &app
                .world()
                .get::<MeshMaterial3d<StandardMaterial>>(mesh)
                .unwrap()
                .0;
            let emits = app
                .world()
                .resource::<Assets<StandardMaterial>>()
                .get(handle)
                .unwrap()
                .emissive;
            [emits.red, emits.green, emits.blue]
        };

        assert_eq!(
            write(&mut app, "glow", Decoded::Scalar(4.0)),
            [2.0, 1.0, 0.0],
            "the GLB's colour, scaled"
        );
        assert_eq!(
            write(&mut app, "emissive", Decoded::Triple([0.0, 0.5, 1.0])),
            [0.0, 2.0, 4.0],
            "a colour written after an intensity is scaled by it"
        );
        assert_eq!(
            write(&mut app, "emissiveIntensity", Decoded::Scalar(0.0)),
            [0.0; 3]
        );
        assert_eq!(
            write(&mut app, "emissiveIntensity", Decoded::Scalar(1.0)),
            [0.0, 0.5, 1.0],
            "the colour comes back"
        );
        assert_eq!(
            write(&mut app, "emissive.r", Decoded::Scalar(0.25)),
            [0.25, 0.5, 1.0]
        );
        // The animatable again, onto a copy of the material the element
        // writes made its own: the colour, not the scaled emission, is kept.
        assert_eq!(
            write(&mut app, "emissiveIntensity", Decoded::Scalar(3.0)),
            [0.75, 1.5, 3.0]
        );
        assert_eq!(
            write(&mut app, "glow", Decoded::Scalar(2.0)),
            [0.5, 1.0, 2.0]
        );
    }

    /// A robot that goes takes the material copies made for it, so they do
    /// not outlive its meshes.
    #[test]
    fn a_robot_that_goes_takes_its_material_copies() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<StandardMaterial>()
            .add_message::<RequestRedraw>()
            .add_plugins(ValuesPlugin);
        let material = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());
        let world = app.world_mut();
        let mesh = world.spawn(MeshMaterial3d(material.clone())).id();
        let mut bindings = SceneBindings::default();
        bindings.by_id.insert(
            "colour".into(),
            vec![SceneBinding {
                entity: mesh,
                feature: FeatureKind::Color,
                axis: None,
                morph_index: None,
            }],
        );
        bindings.indexed = true;
        let root = world
            .spawn((
                asset("a"),
                EntityInstance {
                    id: "a".into(),
                    asset_path: String::new(),
                    meta: Default::default(),
                    bindings,
                    camera: Entity::PLACEHOLDER,
                    draw_order: 0,
                    ready: true,
                },
            ))
            .id();
        world.resource_mut::<ValueWrites>().0.push(ValueWrite {
            instance: Some("a".into()),
            target: "colour".into(),
            feature: None,
            value: Decoded::Triple([1.0, 0.0, 0.0]),
            stamp: None,
        });
        app.update();
        let copy = app
            .world()
            .get::<MeshMaterial3d<StandardMaterial>>(mesh)
            .unwrap()
            .0
            .id();
        assert_ne!(copy, material.id(), "the write made a copy");

        app.world_mut().despawn(mesh);
        app.world_mut().despawn(root);
        for _ in 0..3 {
            app.update();
        }
        assert!(
            app.world()
                .resource::<Assets<StandardMaterial>>()
                .get(copy)
                .is_none(),
            "the copy went with its robot"
        );
    }

    /// A write lands on the robot it names and no other; one for a robot
    /// still loading waits for it; one for an instance nothing is loading is
    /// dropped, a robot that loaded but will never be indexed included.
    #[test]
    fn a_write_reaches_the_instance_it_names() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<StandardMaterial>()
            .add_message::<RequestRedraw>()
            .add_plugins(ValuesPlugin);
        let a = showing(&mut app, "a");
        let b = showing(&mut app, "b");
        // As a load spawns it: incoming, and marked loading until indexed.
        app.world_mut().spawn((
            asset("c"),
            Incoming,
            arora_visuals::instance::Loading {
                instance: "c".into(),
            },
        ));

        // Loaded, never indexed, and so no longer loading.
        app.world_mut().spawn(asset("d"));

        write(&mut app, "a", 0.5);
        write(&mut app, "c", 0.25);
        write(&mut app, "d", 0.5);
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
