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
    pub feature: FeatureKind,
    pub value: Decoded,
}

/// Writes waiting for the next frame, in the order they were pushed.
#[derive(Resource, Default)]
pub struct ValueWrites(pub Vec<ValueWrite>);

/// How many writes the last frame applied: what a host reads to know a push
/// has reached the world.
#[derive(Resource, Default)]
pub struct AppliedValues(pub usize);

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

#[allow(clippy::too_many_arguments)]
pub fn apply_values(
    mut writes: ResMut<ValueWrites>,
    mut applied: ResMut<AppliedValues>,
    instances: Query<(&EntityInstance, Option<&JointRig>)>,
    mut transforms: Query<&mut Transform>,
    mut weights: Query<&mut MorphWeights>,
    mut material_handles: Query<&mut MeshMaterial3d<StandardMaterial>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut joint_gizmos: Query<&mut JointGizmo>,
    mut own: Local<OwnMaterials>,
    mut redraw: MessageWriter<RequestRedraw>,
) {
    applied.0 = 0;
    // Before the robot has spawned there is nowhere for a write to land, and
    // a host may well write as soon as the scene starts: keep the writes for
    // the frame the robot arrives in, rather than dropping them.
    if writes.0.is_empty() || instances.is_empty() {
        return;
    }
    let mut count = 0;
    for write in writes.0.drain(..) {
        for (instance, rig) in &instances {
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
                Some(found) => found.clone(),
                None => match bindings.elements.get(&write.target) {
                    Some(element) => {
                        let entity = match &write.feature {
                            FeatureKind::Translation
                            | FeatureKind::Rotation
                            | FeatureKind::Scale => element.node,
                            _ => match element.mesh {
                                Some(mesh) => mesh,
                                None => continue,
                            },
                        };
                        // An element written directly is one element, but
                        // the GLB may share its material with others, which a
                        // material write would change too. It gets a copy of
                        // its own the first time.
                        if is_material(&write.feature) && own.0.insert(entity) {
                            if let Ok(mut handle) = material_handles.get_mut(entity) {
                                if let Some(copy) = materials.get(&handle.0).cloned() {
                                    handle.0 = materials.add(copy);
                                }
                            }
                        }
                        vec![SceneBinding {
                            entity,
                            feature: write.feature.clone(),
                            axis: None,
                            morph_index: None,
                        }]
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
    }
    applied.0 = count;
    if count > 0 {
        redraw.write(RequestRedraw);
    }
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
