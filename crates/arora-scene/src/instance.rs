//! Making each robot an entity instance the shared layer can work on.
//!
//! The scene spawns a robot as a glTF scene under a root entity. What the
//! shared layer wants is that root carrying an
//! [`EntityInstance`](arora_visuals::EntityInstance): the asset's declared
//! elements joined to the entities they actually became, so a pick or an
//! outline can address an element by the id `RobotData` gives it rather than
//! by whatever Bevy called the node.
//!
//! **Every join is scoped to its own root.** A world holding several robots —
//! several of one asset among them — has several entities under each glTF
//! node name, so a lookup across the whole world binds an element to whichever
//! robot it reaches first — silently, because a name collision is a successful
//! lookup. The walk here is over one root's descendants, with the declaration
//! and joints that root's own [`RobotAsset`] carries, and the instance takes
//! the host's id for it.

use arora_visuals::binding::{ElementTargets, SceneBinding, SceneBindings};
use arora_visuals::instance::{EntityInstance, Loading};
use arora_visuals::meta::FeatureKind;
use bevy::ecs::system::SystemParam;
use bevy::mesh::morph::MorphWeights;
use bevy::prelude::*;

use crate::joints::{JointBinding, JointRig};
use crate::robots::RobotAsset;
use crate::OrbitCamera;

/// The spawned scene, as the join reads it.
#[derive(SystemParam)]
pub struct SpawnedScene<'w, 's> {
    children: Query<'w, 's, &'static Children>,
    names: Query<'w, 's, &'static Name>,
    meshes: Query<'w, 's, Entity, With<Mesh3d>>,
    morphs: Query<'w, 's, Entity, With<MorphWeights>>,
    transforms: Query<'w, 's, &'static Transform>,
}

/// A robot root not indexed yet. An incoming robot is indexed once its swap
/// is finished, so until then values, controls and picks keep addressing the
/// robot that is showing in its instance.
type Unindexed = (
    With<WorldAssetRoot>,
    Without<EntityInstance>,
    Without<crate::robots::Incoming>,
);

/// Joins each spawned robot's elements to its entities, once its scene exists.
///
/// Runs until each root is indexed, because a glTF scene spawns
/// asynchronously and there is no event that says all of it has arrived.
#[allow(clippy::too_many_arguments)]
pub fn index_instances(
    camera: Query<Entity, With<OrbitCamera>>,
    roots: Query<(Entity, &RobotAsset), Unindexed>,
    scene: SpawnedScene,
    mut commands: Commands,
) {
    let Ok(camera) = camera.single() else {
        return;
    };
    for (root, asset) in &roots {
        let id = &asset.instance;
        let meta = &asset.meta;
        let mut by_name = std::collections::HashMap::new();
        for entity in scene.children.iter_descendants(root) {
            if let Ok(name) = scene.names.get(entity) {
                by_name.entry(name.as_str().to_string()).or_insert(entity);
            }
        }
        // Its scene has not spawned yet; a later frame will catch it.
        if by_name.is_empty() {
            continue;
        }

        let mut elements = std::collections::HashMap::new();
        for element in &meta.elements {
            let Some(&node) = by_name.get(&element.node_name) else {
                continue;
            };
            let own = std::iter::once(node).chain(scene.children.iter_descendants(node));
            elements.insert(
                element.id.clone(),
                ElementTargets {
                    node,
                    mesh: scene.meshes.get(node).ok().or_else(|| {
                        scene
                            .children
                            .iter_descendants(node)
                            .find(|e| scene.meshes.contains(*e))
                    }),
                    morph: own.into_iter().find(|e| scene.morphs.contains(*e)),
                },
            );
        }
        // Named descendants but no element among them: the scene is still
        // arriving. Worth saying at debug, because the other reading — a GLB
        // whose declaration names nodes it does not contain — looks identical
        // from here and would otherwise be silent forever.
        if elements.is_empty() {
            log::debug!(
                "{id}: {} named descendants, none of the {} declared nodes",
                by_name.len(),
                meta.elements.len()
            );
            continue;
        }
        log::info!(
            "{id}: joined {}/{} declared elements",
            elements.len(),
            meta.elements.len()
        );

        // Every place each animatable lands: its nodes for a transform
        // feature, the mesh at or under each node for a material one.
        let mut by_id: std::collections::HashMap<String, Vec<SceneBinding>> =
            std::collections::HashMap::new();
        for (animatable, binding) in &meta.animatables {
            // A morph target is found by name among a mesh's targets, which
            // this scene does not read; a robot's declaration carries none.
            if matches!(binding.feature, FeatureKind::Morph(_)) {
                continue;
            }
            let transform = matches!(
                binding.feature,
                FeatureKind::Translation | FeatureKind::Rotation | FeatureKind::Scale
            );
            for node_name in &binding.node_names {
                let Some(&node) = by_name.get(node_name) else {
                    continue;
                };
                // A material feature lands on every primitive the node draws
                // with, each of which has a material of its own.
                let entities = if transform {
                    vec![node]
                } else {
                    crate::values::primitives(node, &scene.children, |e| scene.meshes.contains(e))
                };
                for entity in entities {
                    by_id
                        .entry(animatable.clone())
                        .or_default()
                        .push(SceneBinding {
                            entity,
                            feature: binding.feature.clone(),
                            axis: binding.axis,
                            morph_index: None,
                        });
                }
            }
        }

        // Every joint whose value is animated, bound to the body it moves
        // as that body stands now: the rest every pose is taken from.
        let mut rig = JointRig::default();
        for joint in &asset.joints {
            let (Some(animatable), Some(body)) = (
                &joint.animatable,
                elements.get(&joint.child).map(|e| e.node),
            ) else {
                continue;
            };
            let Ok(rest) = scene.transforms.get(body) else {
                continue;
            };
            if let Some(binding) = JointBinding::new(joint, body, *rest) {
                rig.0.entry(animatable.clone()).or_default().push(binding);
            }
        }

        commands
            .entity(root)
            .remove::<Loading>()
            .insert(rig)
            .insert(EntityInstance {
                id: id.clone(),
                asset_path: asset.glb.clone(),
                meta: meta.clone(),
                bindings: SceneBindings {
                    by_id,
                    elements,
                    indexed: true,
                },
                // Every instance shares the scene's one camera, where a face
                // renderer gives each of its own. That is what `drawn` cannot
                // accommodate — see the note below.
                camera,
                draw_order: 0,
                // Set here rather than earned, because the shared layer's
                // readiness is decided by `drawn`, which moves an instance's
                // camera from below the clear to above it. With one camera over
                // every instance there is no per-instance order to move, so this
                // scene cannot use that mechanism and claims readiness outright.
                ready: true,
            });
    }
}

#[cfg(test)]
mod tests {
    use bevy::world_serialization::WorldAsset;

    use super::*;

    /// Indexing a robot takes off the mark that said it was coming: left on,
    /// the shared controls would hold a change for a key nothing will add as
    /// though a robot were still on its way, asking for frames for good.
    #[test]
    fn indexing_a_robot_takes_its_loading_mark_off() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_systems(Update, index_instances);
        app.world_mut().spawn(OrbitCamera);
        let mut meta = arora_visuals::meta::AssetMeta::default();
        meta.elements.push(arora_visuals::meta::Element {
            id: "head".into(),
            name: String::new(),
            node_name: "Head".into(),
            kind: "shape".into(),
            material: None,
            morph_targets: Vec::new(),
            primitive: None,
        });
        let root = app
            .world_mut()
            .spawn((
                WorldAssetRoot(Handle::<WorldAsset>::default()),
                crate::robots::RobotAsset {
                    instance: "a".into(),
                    key: None,
                    glb: String::new(),
                    meta,
                    joints: Vec::new(),
                },
                Loading {
                    instance: "a".into(),
                },
                Transform::default(),
            ))
            .id();
        app.world_mut()
            .spawn((Name::new("Head"), Transform::default(), ChildOf(root)));
        app.update();
        assert!(app.world().get::<EntityInstance>(root).is_some(), "indexed");
        assert!(
            app.world().get::<Loading>(root).is_none(),
            "no longer coming"
        );
    }
}
