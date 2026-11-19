//! The robots a scene shows: loaded, swapped, placed and unloaded by
//! instance.
//!
//! **A host names each robot by an instance id** — its own string, never
//! rewritten: an Arora path, a slot name, whatever the host uses. Loading
//! under an id the scene already shows swaps that robot; the one showing keeps
//! drawing until its replacement has loaded whole, so a swap never shows an
//! empty space. A second load asked for the same id while one is still loading
//! replaces that one, which is reported [`REPLACED`]. Loads under different
//! ids are independent: each instance loads, swaps and fails on its own.
//!
//! A host asks through [`RobotRequests`] and hears back through
//! [`RobotOutcomes`], one outcome per load, in the order they settled.
//!
//! **Requests act in the order a host asked them.** A load, an unload and a
//! pose asked for between two frames act on what the ones before them left:
//! a load then an unload of one instance leaves it empty, two loads leave the
//! later one loading, and a pose after an unload finds nothing to move.
//!
//! **Where a robot stands is the host's.** A load may carry a pose, and
//! [`RobotRequest::Pose`] moves a robot loaded or loading; without either it
//! stands at the origin. Poses are in the driver's frame — Z up, metres — as
//! the controls' are ([`Pose`]).
//!
//! What a swap takes away with the old robot: its entities, and the controls on
//! its joints and elements. A selection is the host's and is kept, by the
//! instance's id, since a swap is usually a newer version of the same robot,
//! whose element ids carry over; goals are nobody's and stay. An unload takes
//! the selection away too: an id loaded later is a new instance, and must not
//! light up what an old one had selected.

use bevy::asset::RecursiveDependencyLoadState;
use bevy::prelude::*;

use arora_visuals::joint_gizmo::JointGizmo;
use arora_visuals::meta::AssetMeta;
use arora_visuals::select::Selections;
use arora_visuals::transform_gizmo::TransformGizmo;

use crate::controls::Pose;
use crate::robot::{self, Joint};

/// On a robot's root: the instance it is, and the asset it shows.
#[derive(Component, Clone)]
pub struct RobotAsset {
    /// The host's name for the instance.
    pub instance: String,
    /// The host's name for the content, if it gave one: what a host compares
    /// to decide whether a load is a swap or a repeat.
    pub key: Option<String>,
    /// Where its GLB is served from.
    pub glb: String,
    /// What the GLB declares.
    pub meta: AssetMeta,
    /// Its joints, as the GLB declares them.
    pub joints: Vec<Joint>,
}

/// A robot to load into an instance.
#[derive(Clone)]
pub struct RobotLoad {
    pub asset: RobotAsset,
    /// Where it stands, in the driver's frame. `None` keeps the pose of the
    /// robot it replaces, or the origin for a new instance.
    pub pose: Option<Pose>,
}

impl RobotLoad {
    /// A load of the GLB served at `glb` with `bytes`, reading what it
    /// declares from the bytes. A robot whose joints cannot be read still
    /// loads; its joints stay where the GLB put them.
    pub fn read(
        instance: String,
        key: Option<String>,
        glb: String,
        bytes: &[u8],
    ) -> anyhow::Result<Self> {
        let meta = AssetMeta::from_glb_bytes(bytes)?;
        let joints = robot::find_joints(bytes).unwrap_or_else(|error| {
            log::warn!("could not read the joints of {glb}: {error:#}");
            Vec::new()
        });
        Ok(Self {
            asset: RobotAsset {
                instance,
                key,
                glb,
                meta,
                joints,
            },
            pose: None,
        })
    }
}

/// One thing a host asks of an instance's robot.
#[derive(Clone)]
pub enum RobotRequest {
    /// Load a robot into its instance.
    Load(RobotLoad),
    /// Take an instance away, with anything loading into it.
    Unload(String),
    /// Stand an instance's robot, and anything loading into it, at a pose.
    Pose(String, Pose),
}

/// What a host asks of the scene's robots, taken at the start of each frame.
#[derive(Resource, Default)]
pub struct RobotRequests {
    /// In the order the host asked them.
    pub robots: Vec<RobotRequest>,
    /// Frame these instances, or every robot for none: the camera moves to
    /// fit them, as it fits the robots present at the start.
    pub frame: Option<Vec<String>>,
}

/// How one load ended.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// Loaded whole and showing in its instance.
    Loaded {
        instance: String,
        key: Option<String>,
        glb: String,
        joints: Vec<Joint>,
        /// Its elements, as `(id, name)`: the name is what the author called
        /// the element (a link's `base_link`), empty where unnamed.
        elements: Vec<(String, String)>,
    },
    /// Not loaded, and why: the GLB failed to load, or a later load into the
    /// same instance replaced it first ([`REPLACED`]), or the instance was
    /// unloaded first. Whatever the instance showed before still shows.
    Failed {
        instance: String,
        key: Option<String>,
        glb: String,
        error: String,
    },
}

impl Outcome {
    pub fn instance(&self) -> &str {
        match self {
            Outcome::Loaded { instance, .. } | Outcome::Failed { instance, .. } => instance,
        }
    }

    pub fn glb(&self) -> &str {
        match self {
            Outcome::Loaded { glb, .. } | Outcome::Failed { glb, .. } => glb,
        }
    }
}

/// Every load's outcome, in the order they settled, until a host takes them.
#[derive(Resource, Default)]
pub struct RobotOutcomes(pub Vec<Outcome>);

/// A robot loading in the background into its instance, hidden. It is not
/// indexed, drawn or driven until it has loaded whole.
#[derive(Component, Clone)]
pub struct Incoming;

/// The error a load reports when a later load into the same instance replaced
/// it before it finished.
pub const REPLACED: &str = "replaced by a later robot before it finished loading";

/// The error a load reports when its instance was unloaded before it finished.
pub const UNLOADED: &str = "its instance was unloaded before it finished loading";

/// An asset's elements as `(id, name)`.
fn named(meta: &AssetMeta) -> Vec<(String, String)> {
    meta.elements
        .iter()
        .map(|element| (element.id.clone(), element.name.clone()))
        .collect()
}

/// What one instance holds while a frame's requests are taken.
#[derive(Default)]
struct Held {
    /// The robot showing, and where it stands.
    showing: Option<(Entity, Transform)>,
    /// The robot loading into it, and what it is.
    incoming: Option<(Entity, RobotAsset)>,
}

/// Takes the host's requests, in the order it asked them.
///
/// Each acts on what the ones before it left, which the queries cannot say:
/// a request acts through commands, which land only once every request has
/// been taken. So the instances are followed here, request by request — a
/// robot spawned by one load is there for the next to replace, and one
/// despawned by an unload is gone for the pose after it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn take_requests(
    mut requests: ResMut<RobotRequests>,
    mut commands: Commands,
    assets: Res<AssetServer>,
    roots: Query<(Entity, &RobotAsset, &Transform, Has<Incoming>)>,
    children: Query<&Children>,
    transform_gizmos: Query<(Entity, &TransformGizmo)>,
    joint_gizmos: Query<(Entity, &JointGizmo)>,
    mut outcomes: ResMut<RobotOutcomes>,
    mut selections: ResMut<Selections>,
) {
    if requests.robots.is_empty() {
        return;
    }
    let mut held: std::collections::HashMap<String, Held> = std::collections::HashMap::new();
    for (root, asset, transform, incoming) in &roots {
        let at = held.entry(asset.instance.clone()).or_default();
        if incoming {
            at.incoming = Some((root, asset.clone()));
        } else {
            at.showing = Some((root, *transform));
        }
    }
    let mut gone = std::collections::HashSet::new();
    for request in std::mem::take(&mut requests.robots) {
        match request {
            RobotRequest::Unload(instance) => {
                if selections.0.contains_key(&instance) {
                    selections.0.remove(&instance);
                }
                let Some(was) = held.remove(&instance) else {
                    continue;
                };
                if let Some((root, asset)) = was.incoming {
                    outcomes.0.push(failed(&asset, UNLOADED));
                    commands.entity(root).despawn();
                }
                if let Some((root, _)) = was.showing {
                    gone.insert(root);
                    gone.extend(children.iter_descendants(root));
                    commands.entity(root).despawn();
                }
                log::info!("unloaded {instance}");
            }
            RobotRequest::Load(load) => {
                let asset = load.asset;
                let at = held.entry(asset.instance.clone()).or_default();
                // A load still under way into this instance is replaced; the
                // robot showing in it stays until this one has loaded.
                if let Some((root, earlier)) = at.incoming.take() {
                    outcomes.0.push(failed(&earlier, REPLACED));
                    commands.entity(root).despawn();
                }
                let transform = load
                    .pose
                    .map(|pose| pose.to_scene())
                    .or(at.showing.map(|(_, transform)| transform))
                    .unwrap_or_default();
                let scene = assets.load(GltfAssetLabel::Scene(0).from_asset(asset.glb.clone()));
                log::info!("loading {} into {}", asset.glb, asset.instance);
                let root = commands
                    .spawn((
                        WorldAssetRoot(scene),
                        transform,
                        asset.clone(),
                        Incoming,
                        Visibility::Hidden,
                    ))
                    .id();
                at.incoming = Some((root, asset));
            }
            RobotRequest::Pose(instance, pose) => {
                let Some(at) = held.get_mut(&instance) else {
                    continue;
                };
                let to = pose.to_scene();
                if let Some((root, transform)) = &mut at.showing {
                    *transform = to;
                    commands.entity(*root).insert(to);
                }
                if let Some((root, _)) = &at.incoming {
                    commands.entity(*root).insert(to);
                }
            }
        }
    }
    despawn_gizmos_on(&gone, &transform_gizmos, &joint_gizmos, &mut commands);
}

/// Finishes each load once its robot has loaded whole and spawned, taking
/// away the robot it replaces; or abandons it if it failed, keeping that one.
#[allow(clippy::too_many_arguments)]
pub(crate) fn finish_loads(
    mut commands: Commands,
    assets: Res<AssetServer>,
    incoming: Query<(Entity, &WorldAssetRoot, &RobotAsset, Has<Children>), With<Incoming>>,
    showing: Query<(Entity, &RobotAsset), Without<Incoming>>,
    children: Query<&Children>,
    transform_gizmos: Query<(Entity, &TransformGizmo)>,
    joint_gizmos: Query<(Entity, &JointGizmo)>,
    mut outcomes: ResMut<RobotOutcomes>,
    mut loaded: MessageWriter<RobotLoaded>,
) {
    for (root, scene, asset, spawned) in &incoming {
        if let Some(RecursiveDependencyLoadState::Failed(error)) =
            assets.get_recursive_dependency_load_state(scene.0.id())
        {
            log::warn!(
                "could not load {} into {}: {error}",
                asset.glb,
                asset.instance
            );
            outcomes.0.push(failed(asset, &error.to_string()));
            commands.entity(root).despawn();
            continue;
        }
        // A scene spawns its entities once everything it depends on has
        // arrived, so children mean it is whole.
        if !spawned {
            continue;
        }
        let mut old = std::collections::HashSet::new();
        for (previous, held) in &showing {
            if held.instance == asset.instance {
                old.insert(previous);
                old.extend(children.iter_descendants(previous));
                commands.entity(previous).despawn();
            }
        }
        despawn_gizmos_on(&old, &transform_gizmos, &joint_gizmos, &mut commands);
        commands
            .entity(root)
            .remove::<Incoming>()
            .insert((Visibility::Inherited, crate::ready::Drawing));
        outcomes.0.push(Outcome::Loaded {
            instance: asset.instance.clone(),
            key: asset.key.clone(),
            glb: asset.glb.clone(),
            joints: asset.joints.clone(),
            elements: named(&asset.meta),
        });
        loaded.write(RobotLoaded {
            instance: asset.instance.clone(),
        });
        log::info!("{} now shows {}", asset.instance, asset.glb);
    }
}

/// A robot that finished loading into its instance this frame, for the
/// scene's own systems; a host reads [`RobotOutcomes`].
#[derive(Message, Clone, Debug)]
pub struct RobotLoaded {
    pub instance: String,
}

fn failed(asset: &RobotAsset, error: &str) -> Outcome {
    Outcome::Failed {
        instance: asset.instance.clone(),
        key: asset.key.clone(),
        glb: asset.glb.clone(),
        error: error.to_string(),
    }
}

/// Despawns every gizmo whose target is among `gone`: the controls on a robot
/// taken away go with it.
fn despawn_gizmos_on(
    gone: &std::collections::HashSet<Entity>,
    transform_gizmos: &Query<(Entity, &TransformGizmo)>,
    joint_gizmos: &Query<(Entity, &JointGizmo)>,
    commands: &mut Commands,
) {
    if gone.is_empty() {
        return;
    }
    for (gizmo, control) in transform_gizmos {
        if gone.contains(&control.target) {
            commands.entity(gizmo).despawn();
        }
    }
    for (gizmo, control) in joint_gizmos {
        if gone.contains(&control.target) {
            commands.entity(gizmo).despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use bevy::world_serialization::WorldAsset;

    use super::*;

    fn asset(instance: &str, glb: &str) -> RobotAsset {
        RobotAsset {
            instance: instance.into(),
            key: None,
            glb: glb.into(),
            meta: AssetMeta::default(),
            joints: Vec::new(),
        }
    }

    /// An app that runs `systems` each update, with what they read.
    fn app<M>(systems: impl IntoScheduleConfigs<bevy::ecs::system::ScheduleSystem, M>) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<WorldAsset>()
            .init_resource::<RobotRequests>()
            .init_resource::<RobotOutcomes>()
            .init_resource::<Selections>()
            .add_message::<RobotLoaded>()
            .add_systems(Update, systems);
        app
    }

    /// A robot root already spawned whole, showing or `incoming`.
    fn root(app: &mut App, asset: RobotAsset, incoming: bool) -> (Entity, Entity) {
        let world = app.world_mut();
        let mut root = world.spawn((
            WorldAssetRoot(Handle::default()),
            asset,
            Transform::default(),
        ));
        if incoming {
            root.insert(Incoming);
        }
        let root = root.id();
        let body = world.spawn((Transform::default(), ChildOf(root))).id();
        (root, body)
    }

    fn outcomes(app: &mut App) -> Vec<(String, Result<String, String>)> {
        std::mem::take(&mut app.world_mut().resource_mut::<RobotOutcomes>().0)
            .into_iter()
            .map(|outcome| match outcome {
                Outcome::Loaded { instance, glb, .. } => (instance, Ok(glb)),
                Outcome::Failed {
                    instance, error, ..
                } => (instance, Err(error)),
            })
            .collect()
    }

    /// A robot loaded into one instance replaces that instance's robot, and
    /// the controls on it, and leaves every other instance as it was.
    #[test]
    fn a_load_swaps_its_own_instance_only() {
        let mut app = app(finish_loads);
        let (old_a, old_a_body) = root(&mut app, asset("a", "a1.glb"), false);
        let (b, b_body) = root(&mut app, asset("b", "b1.glb"), false);
        let (new_a, _) = root(&mut app, asset("a", "a2.glb"), true);
        let camera = app.world_mut().spawn_empty().id();
        let on_old_a = app
            .world_mut()
            .spawn(TransformGizmo::new(old_a_body, camera))
            .id();
        let on_b = app
            .world_mut()
            .spawn(TransformGizmo::new(b_body, camera))
            .id();
        app.update();

        assert!(app.world().get_entity(old_a).is_err(), "the old robot went");
        assert!(app.world().get_entity(on_old_a).is_err(), "and its control");
        assert!(app.world().get_entity(b).is_ok(), "b is untouched");
        assert!(app.world().get_entity(on_b).is_ok());
        assert!(app.world().get::<Incoming>(new_a).is_none());
        assert_eq!(
            app.world().get::<Visibility>(new_a),
            Some(&Visibility::Inherited)
        );
        assert_eq!(outcomes(&mut app), vec![("a".into(), Ok("a2.glb".into()))]);
    }

    /// A robot not yet spawned whole stays loading, and the one it will
    /// replace stays showing.
    #[test]
    fn a_robot_still_spawning_replaces_nothing_yet() {
        let mut app = app(finish_loads);
        let (old, _) = root(&mut app, asset("a", "a1.glb"), false);
        let loading = app
            .world_mut()
            .spawn((
                WorldAssetRoot(Handle::default()),
                asset("a", "a2.glb"),
                Incoming,
            ))
            .id();
        app.update();
        assert!(app.world().get_entity(old).is_ok());
        assert!(app.world().get::<Incoming>(loading).is_some());
        assert!(outcomes(&mut app).is_empty());
    }

    /// A second load into an instance replaces the one still loading there,
    /// keeps the robot showing, and stands where that robot stands unless it
    /// brings a pose of its own.
    #[test]
    fn a_later_load_replaces_one_still_loading_into_the_same_instance() {
        let mut app = app(take_requests);
        let (showing, _) = root(&mut app, asset("a", "a1.glb"), false);
        let placed = Transform::from_xyz(1.0, 0.0, 2.0);
        app.world_mut().entity_mut(showing).insert(placed);
        let (earlier, _) = root(&mut app, asset("a", "a2.glb"), true);
        ask(
            &mut app,
            [RobotRequest::Load(RobotLoad {
                asset: asset("a", "a3.glb"),
                pose: None,
            })],
        );
        app.update();

        assert!(app.world().get_entity(earlier).is_err());
        assert!(app.world().get_entity(showing).is_ok());
        assert_eq!(outcomes(&mut app), vec![("a".into(), Err(REPLACED.into()))]);
        let mut loading = app
            .world_mut()
            .query_filtered::<(&RobotAsset, &Transform), With<Incoming>>();
        let (asset, at) = loading.single(app.world()).unwrap();
        assert_eq!((asset.glb.as_str(), *at), ("a3.glb", placed));
    }

    /// Unloading takes an instance's robot, its controls and anything loading
    /// into it away, and no other instance's; a pose moves only its own
    /// instance's robot.
    #[test]
    fn unloads_and_poses_reach_their_own_instance_only() {
        let mut app = app(take_requests);
        let (a, a_body) = root(&mut app, asset("a", "a1.glb"), false);
        let (a_loading, _) = root(&mut app, asset("a", "a2.glb"), true);
        let (b, _) = root(&mut app, asset("b", "b1.glb"), false);
        let camera = app.world_mut().spawn_empty().id();
        let on_a = app
            .world_mut()
            .spawn(TransformGizmo::new(a_body, camera))
            .id();
        let pose = Pose {
            position: [1.0, 2.0, 3.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
        };
        {
            let mut selections = app.world_mut().resource_mut::<Selections>();
            for instance in ["a", "b"] {
                selections.0.insert(
                    instance.into(),
                    vec![arora_visuals::select::Selection::of("x")],
                );
            }
        }
        ask(
            &mut app,
            [
                RobotRequest::Unload("a".into()),
                RobotRequest::Pose("b".into(), pose),
            ],
        );
        app.update();

        for gone in [a, a_loading, on_a] {
            assert!(app.world().get_entity(gone).is_err());
        }
        assert_eq!(outcomes(&mut app), vec![("a".into(), Err(UNLOADED.into()))]);
        assert_eq!(app.world().get::<Transform>(b), Some(&pose.to_scene()));
        let selections = app.world().resource::<Selections>();
        assert!(!selections.0.contains_key("a"), "its selection went");
        assert!(selections.0.contains_key("b"));
    }

    fn ask(app: &mut App, requests: impl IntoIterator<Item = RobotRequest>) {
        app.world_mut()
            .resource_mut::<RobotRequests>()
            .robots
            .extend(requests);
    }

    fn load(instance: &str, glb: &str, pose: Option<Pose>) -> RobotRequest {
        RobotRequest::Load(RobotLoad {
            asset: asset(instance, glb),
            pose,
        })
    }

    fn at(x: f32) -> Pose {
        Pose {
            position: [x, 0.0, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
        }
    }

    /// The robots loading, as `(instance, glb, transform)`.
    fn loading(app: &mut App) -> Vec<(String, String, Transform)> {
        let mut query = app
            .world_mut()
            .query_filtered::<(&RobotAsset, &Transform), With<Incoming>>();
        query
            .iter(app.world())
            .map(|(asset, at)| (asset.instance.clone(), asset.glb.clone(), *at))
            .collect()
    }

    /// Two loads into one instance between frames leave the later one
    /// loading, and report the earlier replaced before the later settles.
    #[test]
    fn of_two_loads_asked_together_the_later_is_kept() {
        let mut app = app(take_requests);
        ask(
            &mut app,
            [load("a", "a1.glb", None), load("a", "a2.glb", None)],
        );
        app.update();

        let kept: Vec<_> = loading(&mut app)
            .into_iter()
            .map(|(_, glb, _)| glb)
            .collect();
        assert_eq!(kept, vec!["a2.glb".to_string()]);
        assert_eq!(outcomes(&mut app), vec![("a".into(), Err(REPLACED.into()))]);
    }

    /// A load then an unload of one instance between frames leaves it
    /// empty, the load reported unloaded.
    #[test]
    fn a_load_then_an_unload_leaves_the_instance_empty() {
        let mut app = app(take_requests);
        ask(
            &mut app,
            [load("a", "a1.glb", None), RobotRequest::Unload("a".into())],
        );
        app.update();

        assert!(loading(&mut app).is_empty());
        assert_eq!(outcomes(&mut app), vec![("a".into(), Err(UNLOADED.into()))]);
    }

    /// An unload then a load of one instance between frames starts it
    /// afresh: the load stands at the origin, not where the robot taken away
    /// stood.
    #[test]
    fn an_unload_then_a_load_starts_the_instance_afresh() {
        let mut app = app(take_requests);
        let (showing, _) = root(&mut app, asset("a", "a1.glb"), false);
        app.world_mut()
            .entity_mut(showing)
            .insert(Transform::from_xyz(4.0, 0.0, 0.0));
        ask(
            &mut app,
            [RobotRequest::Unload("a".into()), load("a", "a2.glb", None)],
        );
        app.update();

        assert!(app.world().get_entity(showing).is_err());
        assert_eq!(
            loading(&mut app),
            vec![("a".into(), "a2.glb".into(), Transform::default())]
        );
    }

    /// A pose then an unload of one instance between frames moves nothing:
    /// the robot is gone before the pose would land.
    #[test]
    fn a_pose_then_an_unload_moves_nothing() {
        let mut app = app(take_requests);
        let (showing, _) = root(&mut app, asset("a", "a1.glb"), false);
        ask(
            &mut app,
            [
                RobotRequest::Pose("a".into(), at(1.0)),
                RobotRequest::Unload("a".into()),
            ],
        );
        app.update();
        assert!(app.world().get_entity(showing).is_err());
    }

    /// An unload then a pose of one instance between frames moves nothing,
    /// and asks nothing of the robot taken away.
    #[test]
    fn a_pose_after_an_unload_finds_nothing_to_move() {
        let mut app = app(take_requests);
        let (showing, _) = root(&mut app, asset("a", "a1.glb"), false);
        ask(
            &mut app,
            [
                RobotRequest::Unload("a".into()),
                RobotRequest::Pose("a".into(), at(1.0)),
            ],
        );
        app.update();
        assert!(app.world().get_entity(showing).is_err());
    }

    /// A pose asked for with a load, before it has loaded, places it.
    #[test]
    fn a_pose_reaches_a_robot_still_loading() {
        let mut app = app(take_requests);
        ask(
            &mut app,
            [
                load("a", "a1.glb", Some(at(1.0))),
                RobotRequest::Pose("a".into(), at(2.0)),
            ],
        );
        app.update();
        assert_eq!(
            loading(&mut app),
            vec![("a".into(), "a1.glb".into(), at(2.0).to_scene())]
        );
    }
}
