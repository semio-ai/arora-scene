//! Making each robot an entity instance the shared layer can work on.
//!
//! The scene spawns a robot as a glTF scene under a root entity. What the
//! shared layer wants is that root carrying an
//! [`EntityInstance`](arora_visuals::EntityInstance): the asset's declared
//! elements joined to the entities they actually became, so a pick or an
//! outline can address an element by the id `RobotData` gives it rather than
//! by whatever Bevy called the node.
//!
//! **Every join is scoped to its own root.** A world holding several copies of
//! one robot has several entities under each glTF node name, so a lookup
//! across the whole world binds an element to whichever copy it reaches first
//! — silently, because a name collision is a successful lookup. The walk here
//! is over one root's descendants.

use arora_visuals::binding::{ElementTargets, SceneBindings};
use arora_visuals::instance::EntityInstance;
use arora_visuals::meta::AssetMeta;
use bevy::ecs::system::SystemParam;
use bevy::mesh::morph::MorphWeights;
use bevy::prelude::*;

use crate::OrbitCamera;

/// What the robot's GLB declares, read before the app runs.
///
/// One asset for every copy: the scene spawns the same GLB `n` times, so the
/// declaration is shared and only the joins differ.
#[derive(Resource)]
pub struct RobotMeta(pub AssetMeta);

/// The elements a run outlines, by their `RobotData` ids.
#[derive(Resource, Default)]
pub struct Selected(pub Vec<String>);

/// The spawned scene, as the join reads it.
#[derive(SystemParam)]
pub struct SpawnedScene<'w, 's> {
    children: Query<'w, 's, &'static Children>,
    names: Query<'w, 's, &'static Name>,
    meshes: Query<'w, 's, Entity, With<Mesh3d>>,
    morphs: Query<'w, 's, Entity, With<MorphWeights>>,
}

/// Joins each spawned robot's elements to its entities, once its scene exists.
///
/// Runs until each root is indexed, because a glTF scene spawns
/// asynchronously and there is no event that says all of it has arrived.
pub fn index_instances(
    meta: Res<RobotMeta>,
    selected: Res<Selected>,
    mut selections: ResMut<arora_visuals::glow::Selections>,
    camera: Query<Entity, With<OrbitCamera>>,
    roots: Query<Entity, (With<WorldAssetRoot>, Without<EntityInstance>)>,
    scene: SpawnedScene,
    mut commands: Commands,
) {
    let Ok(camera) = camera.single() else {
        return;
    };
    for (copy, root) in roots.iter().enumerate() {
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
        for element in &meta.0.elements {
            let Some(&node) = by_name.get(&element.node_name) else {
                continue;
            };
            let own = std::iter::once(node).chain(scene.children.iter_descendants(node));
            elements.insert(
                element.id.clone(),
                ElementTargets {
                    node,
                    mesh: scene.meshes.get(node).ok().or_else(|| {
                        scene.children.iter_descendants(node).find(|e| scene.meshes.contains(*e))
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
                "robot {copy}: {} named descendants, none of the {} declared nodes",
                by_name.len(),
                meta.0.elements.len()
            );
            continue;
        }
        log::info!(
            "robot {copy}: joined {}/{} declared elements",
            elements.len(),
            meta.0.elements.len()
        );

        let id = format!("robot-{copy}");
        if !selected.0.is_empty() {
            selections.0.insert(id.clone(), selected.0.clone());
        }
        commands.entity(root).insert(EntityInstance {
            id,
            asset_path: String::new(),
            meta: meta.0.clone(),
            bindings: SceneBindings {
                by_id: std::collections::HashMap::new(),
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
