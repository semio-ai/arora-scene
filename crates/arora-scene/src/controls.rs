//! Controls a driver adds, shows, hides, moves and removes, by keys of its own.
//!
//! The controls are arora-visuals' shared control layer
//! ([`arora_visuals::control`]), the one every host takes: the commands, the
//! events, the rules for the order commands land in, fades, highlights and
//! sizes are all its, so a driver that works one host works the others. **A
//! drag moves nothing by itself**: a joint control reports the value the drag
//! asks for, and a transform control reports the pose, and the robot moves
//! when the driver writes that value or pose.
//!
//! What this module adds is what only the scene knows, told to the shared
//! layer in the places it asks:
//!
//! - **The driver's frame.** The scene is Bevy's own Y up; a driver speaks Z
//!   up. The orbit camera carries [`DriverFrame::y_up`], so poses convert at
//!   this boundary and a transform control's handles are labelled as the
//!   driver names the axes. [`ScenePose`] gives the scene's other writers —
//!   a robot's pose, say — the same conversion.
//! - **Which camera a goal is used through.** The orbit camera is the
//!   [`ControlCamera`]; a control on a robot is used through the robot's own
//!   camera, which is that same camera.
//! - **What is still to come.** A robot loading into an instance carries
//!   [`Loading`](arora_visuals::instance::Loading) until it is indexed, so a
//!   control added in the frames a load takes waits for it rather than being
//!   refused.
//! - **What a joint is.** A joint add is resolved here, from the robot's
//!   declared joints and the rig bound to its bodies ([`resolve_joints`]).
//!
//! The JSON a host sends reads through [`read_command`], which also takes the
//! older name for a whole robot as a control's target.

pub use arora_visuals::control::{
    Control, ControlCamera, ControlCommand, ControlCommands, ControlEvent, ControlEvents,
    ControlPass, ControlPress, ControlPresses, ControlSize, ControlSystems, Controls, DriverFrame,
    HiddenByDriver, JointRequest, Mode, Modifiers, Phase, Pose, PressButton, Target, FADE_SECS,
};
use arora_visuals::joint_gizmo::JointGizmo;
pub use arora_visuals::transform_gizmo::{GizmoDrags, GizmoSize};
use bevy::prelude::*;

use crate::joints::JointRig;
use crate::robots::RobotAsset;
use crate::OrbitCamera;

/// A [`Pose`] in the scene's own world, which is Bevy's Y up: the conversion
/// the controls make, for the scene's other writers of a driver's pose.
pub trait ScenePose {
    /// Where `transform`, in the scene's world, stands as the driver sees it.
    fn from_scene(transform: &Transform) -> Self;
    /// This pose in the scene's world.
    fn to_scene(&self) -> Transform;
}

impl ScenePose for Pose {
    fn from_scene(transform: &Transform) -> Self {
        Pose::from_world(transform, &DriverFrame::y_up())
    }

    fn to_scene(&self) -> Transform {
        self.to_world(&DriverFrame::y_up())
    }
}

/// Reads a command a host sent as JSON.
///
/// A transform control's whole-robot target is `{ "kind": "instance" }`, the
/// name every host's controls take. `{ "kind": "robot" }`, the scene's name
/// for it before the scene took the shared controls, is still read as the
/// same target, and said in the log as deprecated, so a driver written
/// against it keeps working while it moves to the new name.
pub fn read_command(json: &str) -> Result<ControlCommand, serde_json::Error> {
    let mut value: serde_json::Value = serde_json::from_str(json)?;
    let target = value
        .get_mut("control")
        .and_then(|control| control.get_mut("target"))
        .and_then(|target| target.get_mut("kind"));
    if let Some(kind) = target {
        if kind == "robot" {
            log::warn!(
                "a control target of kind \"robot\" is deprecated; send \"instance\" instead"
            );
            *kind = serde_json::Value::from("instance");
        }
    }
    serde_json::from_value(value)
}

/// The scene's side of the shared controls ([module docs](self)). It adds the
/// shared [`ControlsPlugin`](arora_visuals::control::ControlsPlugin); the
/// gizmos' own plugins go beside it.
pub struct ControlsPlugin;

impl Plugin for ControlsPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(arora_visuals::control::ControlsPlugin)
            .add_observer(use_controls_through)
            .add_systems(ControlPass, resolve_joints.in_set(ControlSystems::Joints));
    }
}

/// Makes the orbit camera the one controls are used through, speaking the
/// driver's Z-up frame.
fn use_controls_through(added: On<Add<OrbitCamera>>, mut commands: Commands) {
    commands
        .entity(added.entity)
        .insert((ControlCamera, DriverFrame::y_up()));
}

/// Builds the gizmo of each joint control waiting to be resolved: the joint
/// its robot declares as driven by the requested animatable, bound through
/// the rig to the body it moves, standing at that body's value now, in the
/// joint's own colour. Where a joint has bindings for mimics as well as its
/// own, the control is on its own. A request this cannot resolve — no such
/// joint, or a robot with no rig — is left as it is, and the shared layer
/// refuses the add and says so in the log.
pub fn resolve_joints(
    mut commands: Commands,
    requests: Query<(Entity, &JointRequest), Without<JointGizmo>>,
    robots: Query<(Option<&JointRig>, &RobotAsset)>,
    transforms: Query<&Transform>,
) {
    for (entity, request) in &requests {
        let Ok((rig, asset)) = robots.get(request.instance) else {
            continue;
        };
        let declared = asset
            .joints
            .iter()
            .find(|joint| joint.animatable.as_ref() == Some(&request.joint));
        let binding = rig
            .and_then(|rig| rig.0.get(&request.joint))
            .and_then(|bound| bound.iter().find(|b| b.mimic.is_none()).or(bound.first()));
        let (Some(declared), Some(binding)) = (declared, binding) else {
            continue;
        };
        let current = transforms
            .get(binding.body)
            .map(|transform| binding.value_of(transform))
            .unwrap_or(binding.rest_value);
        let mut gizmo = JointGizmo::new(
            binding.body,
            request.camera,
            binding.motion,
            binding.axis,
            declared.min,
            declared.max,
            current,
            declared.color.into(),
        );
        gizmo.limited = declared.is_limited();
        // A remove in the same frame may have taken the request away.
        commands.entity(entity).try_insert(gizmo);
    }
}

#[cfg(test)]
mod tests {
    use arora_visuals::binding::SceneBindings;
    use arora_visuals::instance::{EntityInstance, Loading};
    use arora_visuals::joint_gizmo::JointDrag;
    use arora_visuals::transform_gizmo::{
        DragPhase, GizmoDrag, GizmoHandle, GizmoPress, GizmoSpace, TransformGizmo,
        TransformGizmoSystems,
    };
    use bevy::picking::pointer::PointerButton;
    use bevy::window::RequestRedraw;

    use super::*;

    fn no_turn() -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }
    use crate::joints::JointBinding;
    use crate::robot::{Joint, JointKind};

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
            min: -0.5,
            max: 0.5,
            default: 0.0,
            color: Srgba::WHITE,
        }
    }

    /// The instance the fixture's robot loads into.
    const ROBOT: &str = "robot-0";

    /// What the fixture's robot declares: the tilt joint.
    fn asset() -> RobotAsset {
        RobotAsset {
            instance: ROBOT.into(),
            key: None,
            glb: String::new(),
            meta: Default::default(),
            joints: vec![tilt()],
        }
    }

    /// A scene with a camera and a body the tilt joint moves, and a robot
    /// loading into `robot-0` but not yet indexed: `robot` indexes it.
    fn scene() -> (App, Entity) {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<RequestRedraw>()
            .add_plugins(ControlsPlugin);
        app.world_mut().spawn(OrbitCamera);
        app.world_mut().spawn((
            asset(),
            crate::robots::Incoming,
            Loading {
                instance: ROBOT.into(),
            },
        ));
        let body = app.world_mut().spawn(Transform::default()).id();
        (app, body)
    }

    fn robot(app: &mut App, body: Entity) {
        spawn_robot(app.world_mut(), body);
    }

    fn spawn_robot(world: &mut World, body: Entity) {
        let camera = world
            .query_filtered::<Entity, With<OrbitCamera>>()
            .single(world)
            .unwrap();
        let mut rig = JointRig::default();
        rig.0.insert(
            TILT.into(),
            vec![JointBinding::new(&tilt(), body, Transform::default()).unwrap()],
        );
        // The robot loading into `robot-0` has loaded: it is no longer
        // incoming, and is indexed.
        let loading = world
            .query_filtered::<Entity, With<crate::robots::Incoming>>()
            .single(world)
            .unwrap();
        world
            .entity_mut(loading)
            .remove::<(crate::robots::Incoming, Loading)>()
            .insert((
                rig,
                EntityInstance {
                    id: ROBOT.into(),
                    asset_path: String::new(),
                    meta: Default::default(),
                    bindings: SceneBindings {
                        indexed: true,
                        ..default()
                    },
                    camera,
                    draw_order: 0,
                    ready: true,
                },
            ));
    }

    /// A second robot, `robot-1`, showing and indexed, with a tilt joint of its
    /// own moving `body`.
    fn second_robot(app: &mut App, body: Entity) {
        let world = app.world_mut();
        let camera = world
            .query_filtered::<Entity, With<OrbitCamera>>()
            .single(world)
            .unwrap();
        let mut rig = JointRig::default();
        rig.0.insert(
            TILT.into(),
            vec![JointBinding::new(&tilt(), body, Transform::default()).unwrap()],
        );
        world.spawn((
            rig,
            RobotAsset {
                instance: "robot-1".into(),
                ..asset()
            },
            EntityInstance {
                id: "robot-1".into(),
                asset_path: String::new(),
                meta: Default::default(),
                bindings: SceneBindings {
                    indexed: true,
                    ..default()
                },
                camera,
                draw_order: 0,
                ready: true,
            },
        ));
    }

    /// Each joint control lands on the joint of the robot it names, though
    /// both robots' joints are driven by one animatable id; one on a robot
    /// nothing is loading is refused rather than kept waiting.
    #[test]
    fn a_joint_control_lands_on_the_robot_it_names() {
        let (mut app, body) = scene();
        robot(&mut app, body);
        let other = app.world_mut().spawn(Transform::default()).id();
        second_robot(&mut app, other);
        let joint_on = |instance: &str| Control::Joint {
            instance: instance.into(),
            joint: TILT.into(),
            color: None,
            size: None,
        };
        for (key, instance) in [("first", ROBOT), ("second", "robot-1"), ("nobody", "ghost")] {
            send(
                &mut app,
                ControlCommand::Add {
                    key: key.into(),
                    control: joint_on(instance),
                },
            );
        }
        let mut gizmos = app.world_mut().query::<&JointGizmo>();
        let mut on: Vec<(String, Entity)> = gizmos
            .iter(app.world())
            .map(|gizmo| (gizmo.key.clone(), gizmo.target))
            .collect();
        on.sort();
        assert_eq!(on, vec![("first".into(), body), ("second".into(), other)]);
        assert!(
            app.world().resource::<ControlCommands>().0.is_empty(),
            "nothing waits for a robot nothing is loading",
        );
    }

    /// A joint control is built from the joint its robot declares and the
    /// body that joint moves: on the joint's own binding where a mimic's is
    /// bound too, standing at the body's value now rather than its rest,
    /// between the declared stops, in the declared colour.
    #[test]
    fn a_joint_control_takes_its_joint_s_binding_value_stops_and_colour() {
        let (mut app, body) = scene();
        robot(&mut app, body);
        let follower = app.world_mut().spawn(Transform::default()).id();
        let declared = Joint {
            min: -0.4,
            max: 0.6,
            color: Srgba::RED,
            ..tilt()
        };
        let mimic = Joint {
            mimic: Some((2.0, 0.0)),
            ..declared.clone()
        };
        let root = app
            .world_mut()
            .query_filtered::<Entity, With<JointRig>>()
            .single(app.world())
            .unwrap();
        let mut rig = JointRig::default();
        rig.0.insert(
            TILT.into(),
            vec![
                JointBinding::new(&mimic, follower, Transform::default()).unwrap(),
                JointBinding::new(&declared, body, Transform::default()).unwrap(),
            ],
        );
        app.world_mut().entity_mut(root).insert(rig);
        app.world_mut().get_mut::<RobotAsset>(root).unwrap().joints = vec![declared];
        app.world_mut().get_mut::<Transform>(body).unwrap().rotation = Quat::from_rotation_x(0.3);
        send(
            &mut app,
            ControlCommand::Add {
                key: "tilt".into(),
                control: Control::Joint {
                    instance: ROBOT.into(),
                    joint: TILT.into(),
                    color: None,
                    size: None,
                },
            },
        );
        let gizmo = only::<JointGizmo>(&mut app).expect("a joint control");
        let gizmo = app.world().get::<JointGizmo>(gizmo).unwrap();
        assert_eq!(gizmo.target, body, "on the joint's own binding");
        assert!((gizmo.value - 0.3).abs() < 1e-5, "at the body's value now");
        assert_eq!((gizmo.min, gizmo.max), (-0.4, 0.6));
        assert!(gizmo.limited);
        assert_eq!(gizmo.colour, Color::from(Srgba::RED));

        // A joint that turns without end is not held to its stops, though
        // it declares them.
        app.world_mut().get_mut::<RobotAsset>(root).unwrap().joints[0].kind = JointKind::Continuous;
        send(
            &mut app,
            ControlCommand::Add {
                key: "spin".into(),
                control: Control::Joint {
                    instance: ROBOT.into(),
                    joint: TILT.into(),
                    color: None,
                    size: None,
                },
            },
        );
        let mut gizmos = app.world_mut().query::<&JointGizmo>();
        let spin = gizmos
            .iter(app.world())
            .find(|gizmo| gizmo.key == "spin")
            .expect("the second control");
        assert!(!spin.limited, "a continuous joint is not limited");
    }

    /// Of two adds under one key, the one asked for last is the control
    /// left, even when the first waits for its robot to load; and a change
    /// sent behind an add that waits acts on the control it makes.
    #[test]
    fn a_later_add_under_a_key_outlasts_one_waiting_for_its_robot() {
        let (mut app, body) = scene();
        let other = app.world_mut().spawn(Transform::default()).id();
        second_robot(&mut app, other);
        for instance in [ROBOT, "robot-1"] {
            app.world_mut()
                .resource_mut::<ControlCommands>()
                .0
                .push(ControlCommand::Add {
                    key: "k".into(),
                    control: Control::Joint {
                        instance: instance.into(),
                        joint: TILT.into(),
                        color: None,
                        size: None,
                    },
                });
        }
        app.update();
        robot(&mut app, body);
        app.update();
        app.update();
        let mut gizmos = app.world_mut().query::<&JointGizmo>();
        let on: Vec<Entity> = gizmos.iter(app.world()).map(|gizmo| gizmo.target).collect();
        assert_eq!(on, vec![other]);
    }

    /// A change sent behind an add waiting for its robot acts on the
    /// control that add makes, not the one under that key it replaces.
    #[test]
    fn a_change_behind_a_waiting_add_reaches_the_control_it_makes() {
        let (mut app, body) = scene();
        let other = app.world_mut().spawn(Transform::default()).id();
        second_robot(&mut app, other);
        let on = |instance: &str| ControlCommand::Add {
            key: "k".into(),
            control: Control::Joint {
                instance: instance.into(),
                joint: TILT.into(),
                color: None,
                size: None,
            },
        };
        send(&mut app, on("robot-1"));
        app.world_mut().resource_mut::<ControlCommands>().0.extend([
            on(ROBOT),
            ControlCommand::Show {
                key: "k".into(),
                visible: false,
            },
        ]);
        app.update();
        let mut hidden = app
            .world_mut()
            .query_filtered::<&JointGizmo, With<HiddenByDriver>>();
        assert_eq!(
            hidden.iter(app.world()).count(),
            0,
            "the old control is left alone"
        );

        robot(&mut app, body);
        app.update();
        app.update();
        let mut gizmos = app
            .world_mut()
            .query::<(&JointGizmo, Has<HiddenByDriver>)>();
        let on: Vec<(Entity, bool)> = gizmos
            .iter(app.world())
            .map(|(gizmo, hidden)| (gizmo.target, hidden))
            .collect();
        assert_eq!(on, vec![(body, true)]);
    }

    /// A remove sent behind an add waiting for its robot takes the control
    /// away at once, the one the add would have replaced included, and the
    /// add with it.
    #[test]
    fn a_remove_behind_a_waiting_add_cancels_both() {
        let (mut app, body) = scene();
        let other = app.world_mut().spawn(Transform::default()).id();
        second_robot(&mut app, other);
        let on = |instance: &str| ControlCommand::Add {
            key: "k".into(),
            control: Control::Joint {
                instance: instance.into(),
                joint: TILT.into(),
                color: None,
                size: None,
            },
        };
        send(&mut app, on("robot-1"));
        app.world_mut()
            .resource_mut::<ControlCommands>()
            .0
            .extend([on(ROBOT), ControlCommand::Remove { key: "k".into() }]);
        app.update();
        assert!(only::<JointGizmo>(&mut app).is_none(), "removed at once");

        robot(&mut app, body);
        app.update();
        app.update();
        assert!(
            only::<JointGizmo>(&mut app).is_none(),
            "and not brought back"
        );
    }

    /// A change waiting for the gizmo of the control its add made acts on
    /// that control, and a later add under the key replaces it afterwards
    /// with nothing carried over.
    #[test]
    fn a_change_for_a_control_still_spawning_is_not_carried_to_its_replacement() {
        let (mut app, body) = scene();
        let other = app.world_mut().spawn(Transform::default()).id();
        second_robot(&mut app, other);
        let on = |instance: &str| ControlCommand::Add {
            key: "k".into(),
            control: Control::Joint {
                instance: instance.into(),
                joint: TILT.into(),
                color: None,
                size: None,
            },
        };
        app.world_mut().resource_mut::<ControlCommands>().0.extend([
            on(ROBOT),
            configure("k", None, Some(false)),
            on("robot-1"),
        ]);
        app.update();
        robot(&mut app, body);
        for _ in 0..3 {
            app.update();
        }
        let mut gizmos = app.world_mut().query::<&JointGizmo>();
        let on: Vec<(Entity, bool)> = gizmos
            .iter(app.world())
            .map(|gizmo| (gizmo.target, gizmo.interactive))
            .collect();
        assert_eq!(on, vec![(other, true)]);
    }

    /// A show for a key nothing has added or will add is dropped, rather than
    /// waiting, and asking for frames, for good.
    #[test]
    fn a_show_for_a_key_nothing_will_add_is_dropped() {
        let (mut app, body) = scene();
        robot(&mut app, body);
        send(
            &mut app,
            ControlCommand::Show {
                key: "nobody".into(),
                visible: false,
            },
        );
        assert!(app.world().resource::<ControlCommands>().0.is_empty());
    }

    /// Every change sent with its add holds from the control's first frame,
    /// however many adds the batch chains under one key.
    #[test]
    fn a_change_holds_from_the_first_frame_through_chained_replacements() {
        let (mut app, body) = scene();
        robot(&mut app, body);
        let other = app.world_mut().spawn(Transform::default()).id();
        second_robot(&mut app, other);
        let on = |instance: &str| ControlCommand::Add {
            key: "k".into(),
            control: Control::Joint {
                instance: instance.into(),
                joint: TILT.into(),
                color: None,
                size: None,
            },
        };
        app.world_mut().resource_mut::<ControlCommands>().0.extend([
            on(ROBOT),
            configure("k", Some(0.5), None),
            on("robot-1"),
            configure("k", Some(0.25), Some(false)),
        ]);
        app.update();
        let mut gizmos = app.world_mut().query::<&JointGizmo>();
        let on: Vec<(Entity, f32, bool)> = gizmos
            .iter(app.world())
            .map(|gizmo| (gizmo.target, gizmo.opacity, gizmo.interactive))
            .collect();
        assert_eq!(on, vec![(other, 0.25, false)]);
        assert!(app.world().resource::<ControlCommands>().0.is_empty());
    }

    /// Changes sent after an add that is refused were for the control it
    /// would have made: they are dropped, not applied to the control it
    /// would have replaced.
    #[test]
    fn changes_after_a_refused_add_do_not_reach_the_control_it_would_replace() {
        let (mut app, body) = scene();
        robot(&mut app, body);
        let joint_on = |instance: &str| ControlCommand::Add {
            key: "k".into(),
            control: Control::Joint {
                instance: instance.into(),
                joint: TILT.into(),
                color: None,
                size: None,
            },
        };
        send(&mut app, joint_on(ROBOT));
        app.world_mut().resource_mut::<ControlCommands>().0.extend([
            joint_on("ghost"),
            ControlCommand::Show {
                key: "k".into(),
                visible: false,
            },
        ]);
        app.update();
        let gizmo = only::<JointGizmo>(&mut app).expect("the old control stays");
        assert_ne!(
            app.world().get::<Visibility>(gizmo),
            Some(&Visibility::Hidden),
            "the show was for the refused add's control"
        );
        assert!(app.world().resource::<ControlCommands>().0.is_empty());
    }

    /// Changes sent before an add that is refused were waiting for it: they
    /// go with it, rather than landing on a later add under the key.
    #[test]
    fn changes_waiting_for_a_refused_add_do_not_reach_a_later_one() {
        let (mut app, body) = scene();
        robot(&mut app, body);
        let joint_on = |instance: &str| ControlCommand::Add {
            key: "k".into(),
            control: Control::Joint {
                instance: instance.into(),
                joint: TILT.into(),
                color: None,
                size: None,
            },
        };
        app.world_mut().resource_mut::<ControlCommands>().0.extend([
            ControlCommand::Show {
                key: "k".into(),
                visible: false,
            },
            joint_on("ghost"),
        ]);
        app.update();
        send(&mut app, joint_on(ROBOT));
        let gizmo = only::<JointGizmo>(&mut app).expect("the later add is placed");
        assert_ne!(
            app.world().get::<Visibility>(gizmo),
            Some(&Visibility::Hidden),
            "the show was for the refused add"
        );
        assert!(app.world().resource::<ControlCommands>().0.is_empty());
    }

    /// An add refused for a joint the robot does not have leaves the control
    /// under its key as it was, with the changes sent for it, and takes the
    /// changes sent for itself.
    #[test]
    fn an_add_refused_for_an_unknown_joint_leaves_the_control_it_would_replace() {
        let (mut app, body) = scene();
        robot(&mut app, body);
        let joint = |name: &str| ControlCommand::Add {
            key: "k".into(),
            control: Control::Joint {
                instance: ROBOT.into(),
                joint: name.into(),
                color: None,
                size: None,
            },
        };
        send(&mut app, joint(TILT));
        let before = only::<JointGizmo>(&mut app).expect("placed");
        app.world_mut()
            .resource_mut::<ControlCommands>()
            .0
            .extend([joint("no-such-joint"), configure("k", None, Some(false))]);
        app.update();
        let after = only::<JointGizmo>(&mut app).expect("the old control stays");
        assert_eq!(after, before);
        assert!(app.world().get::<JointGizmo>(after).unwrap().interactive);
        assert!(app.world().resource::<ControlCommands>().0.is_empty());
    }

    fn send(app: &mut App, command: ControlCommand) {
        app.world_mut()
            .resource_mut::<ControlCommands>()
            .0
            .push(command);
        app.update();
    }

    fn only<C: Component>(app: &mut App) -> Option<Entity> {
        let found: Vec<Entity> = app
            .world_mut()
            .query_filtered::<Entity, With<C>>()
            .iter(app.world())
            .collect();
        assert!(found.len() <= 1, "{} found", found.len());
        found.first().copied()
    }

    fn events(app: &mut App) -> Vec<ControlEvent> {
        std::mem::take(&mut app.world_mut().resource_mut::<ControlEvents>().0)
    }

    #[test]
    fn a_joint_control_reports_its_drag_and_moves_nothing() {
        let (mut app, body) = scene();
        robot(&mut app, body);
        send(
            &mut app,
            ControlCommand::Add {
                key: TILT.into(),
                control: Control::Joint {
                    instance: ROBOT.into(),
                    joint: TILT.into(),
                    color: None,
                    size: None,
                },
            },
        );
        let gizmo = only::<JointGizmo>(&mut app).expect("a joint control");
        let joint = app.world().get::<JointGizmo>(gizmo).unwrap();
        assert_eq!(
            (joint.target, joint.key.as_str(), joint.apply),
            (body, TILT, false)
        );

        app.world_mut().trigger(JointDrag {
            entity: gizmo,
            target: body,
            phase: DragPhase::Move,
            gesture: 0,
            value: 0.3,
        });
        let events = events(&mut app);
        assert_eq!(events.len(), 1);
        assert_eq!((events[0].key.as_str(), events[0].value), (TILT, Some(0.3)));
        assert_eq!(
            *app.world().get::<Transform>(body).unwrap(),
            Transform::default()
        );
    }

    #[test]
    fn a_control_added_before_the_robot_arrives_waits_for_it() {
        let (mut app, body) = scene();
        send(
            &mut app,
            ControlCommand::Add {
                key: TILT.into(),
                control: Control::Joint {
                    instance: ROBOT.into(),
                    joint: TILT.into(),
                    color: None,
                    size: None,
                },
            },
        );
        send(
            &mut app,
            ControlCommand::Show {
                key: TILT.into(),
                visible: false,
            },
        );
        assert!(only::<JointGizmo>(&mut app).is_none());
        robot(&mut app, body);
        app.update();
        let gizmo = only::<JointGizmo>(&mut app).expect("added once the robot arrived");
        assert_eq!(
            app.world().get::<Visibility>(gizmo),
            Some(&Visibility::Hidden)
        );
    }

    #[test]
    fn a_control_whose_gizmo_went_with_its_robot_is_forgotten() {
        let (mut app, body) = scene();
        robot(&mut app, body);
        send(
            &mut app,
            ControlCommand::Add {
                key: TILT.into(),
                control: Control::Joint {
                    instance: ROBOT.into(),
                    joint: TILT.into(),
                    color: None,
                    size: None,
                },
            },
        );
        let gizmo = only::<JointGizmo>(&mut app).unwrap();
        // What a robot swap does to the controls on the old robot.
        app.world_mut().despawn(gizmo);
        send(
            &mut app,
            ControlCommand::Show {
                key: TILT.into(),
                visible: false,
            },
        );
        // Neither shown on a dead entity nor kept: with nothing adding the key
        // and nothing loading, it is dropped rather than asking for frames
        // for good. A show sent before the key is added again waits for that
        // add, as for any control not yet added.
        assert!(
            app.world().resource::<ControlCommands>().0.is_empty(),
            "a show for a key nothing will add is dropped"
        );
    }

    #[test]
    fn a_goal_reports_its_drag_in_the_driver_s_frame_and_moves_when_set() {
        let (mut app, body) = scene();
        robot(&mut app, body);
        let at = Pose {
            position: [1.0, 2.0, 3.0],
            rotation: no_turn(),
        };
        send(
            &mut app,
            ControlCommand::Add {
                key: "goal".into(),
                control: Control::Transform {
                    target: Target::Goal {
                        pose: at,
                        view: None,
                    },
                    modes: None,
                    local: false,
                    colors: None,
                    face_viewer: false,
                    size: None,
                },
            },
        );
        let gizmo = only::<TransformGizmo>(&mut app).expect("a transform control");
        let goal = app.world().get::<TransformGizmo>(gizmo).unwrap().target;
        assert!(!app.world().get::<TransformGizmo>(gizmo).unwrap().apply);
        assert_eq!(*app.world().get::<Transform>(goal).unwrap(), at.to_scene());

        // A drag one metre up the driver's Z, which is the scene's Y.
        let asked = Transform::from_translation(at.to_scene().translation + Vec3::Y);
        app.world_mut().trigger(GizmoDrag {
            entity: gizmo,
            target: goal,
            handle: GizmoHandle::Arrow(2),
            phase: DragPhase::End,
            gesture: 0,
            transform: asked,
        });
        let events = events(&mut app);
        let pose = events[0].pose.expect("a pose");
        assert!(Vec3::from(pose.position).abs_diff_eq(Vec3::new(1.0, 2.0, 4.0), 1e-5));
        assert_eq!(*app.world().get::<Transform>(goal).unwrap(), at.to_scene());

        send(
            &mut app,
            ControlCommand::SetPose {
                key: "goal".into(),
                pose,
                scale: Some([2.0, 1.0, 1.0]),
            },
        );
        let moved = app.world().get::<Transform>(goal).unwrap();
        assert!(moved.translation.abs_diff_eq(asked.translation, 1e-5));
        assert_eq!(moved.scale, Vec3::new(2.0, 1.0, 1.0));

        send(&mut app, ControlCommand::Remove { key: "goal".into() });
        assert!(app.world().get_entity(gizmo).is_err());
        assert!(app.world().get_entity(goal).is_err());
    }

    /// A transform control on the fixture's robot as a whole.
    fn on_robot() -> Control {
        Control::Transform {
            target: Target::Instance {
                instance: ROBOT.into(),
            },
            modes: None,
            local: false,
            colors: None,
            face_viewer: false,
            size: None,
        }
    }

    fn goal_at(position: [f32; 3]) -> Control {
        Control::Transform {
            target: Target::Goal {
                pose: Pose {
                    position,
                    rotation: no_turn(),
                },
                view: None,
            },
            modes: None,
            local: false,
            colors: None,
            face_viewer: false,
            size: None,
        }
    }

    #[test]
    fn a_pose_set_in_the_frame_a_goal_is_added_is_not_lost() {
        let (mut app, body) = scene();
        robot(&mut app, body);
        let moved_to = Pose {
            position: [0.0, 0.0, 2.0],
            rotation: no_turn(),
        };
        app.world_mut().resource_mut::<ControlCommands>().0.extend([
            ControlCommand::Add {
                key: "goal".into(),
                control: goal_at([0.0, 0.0, 1.0]),
            },
            ControlCommand::SetPose {
                key: "goal".into(),
                pose: moved_to,
                scale: None,
            },
        ]);
        app.update();
        app.update();
        let gizmo = only::<TransformGizmo>(&mut app).unwrap();
        let goal = app.world().get::<TransformGizmo>(gizmo).unwrap().target;
        let at = app.world().get::<Transform>(goal).unwrap();
        assert!(at
            .translation
            .abs_diff_eq(moved_to.to_scene().translation, 1e-5));
    }

    #[test]
    fn a_control_removed_before_the_robot_arrives_never_appears() {
        let (mut app, body) = scene();
        app.world_mut().resource_mut::<ControlCommands>().0.extend([
            ControlCommand::Add {
                key: TILT.into(),
                control: Control::Joint {
                    instance: ROBOT.into(),
                    joint: TILT.into(),
                    color: None,
                    size: None,
                },
            },
            ControlCommand::Show {
                key: TILT.into(),
                visible: false,
            },
        ]);
        app.update();
        send(&mut app, ControlCommand::Remove { key: TILT.into() });
        robot(&mut app, body);
        app.update();
        assert!(only::<JointGizmo>(&mut app).is_none());
        assert!(app.world().resource::<ControlCommands>().0.is_empty());
    }

    #[test]
    fn a_hidden_control_is_marked_as_the_driver_s_doing() {
        let (mut app, body) = scene();
        robot(&mut app, body);
        send(
            &mut app,
            ControlCommand::Add {
                key: TILT.into(),
                control: Control::Joint {
                    instance: ROBOT.into(),
                    joint: TILT.into(),
                    color: None,
                    size: None,
                },
            },
        );
        let gizmo = only::<JointGizmo>(&mut app).unwrap();
        let show = |visible| ControlCommand::Show {
            key: TILT.into(),
            visible,
        };
        send(&mut app, show(false));
        assert!(app.world().get::<HiddenByDriver>(gizmo).is_some());
        send(&mut app, show(true));
        assert!(app.world().get::<HiddenByDriver>(gizmo).is_none());
        assert_eq!(
            app.world().get::<Visibility>(gizmo),
            Some(&Visibility::Inherited)
        );
    }

    #[test]
    fn the_scene_s_up_is_the_driver_s_z() {
        let pose = Pose::from_scene(&Transform::from_xyz(1.0, 2.0, 3.0));
        assert!(Vec3::from(pose.position).abs_diff_eq(Vec3::new(1.0, -3.0, 2.0), 1e-6));
    }

    #[test]
    fn a_turn_about_the_scene_s_up_is_a_turn_about_the_driver_s_z() {
        let turn = Quat::from_rotation_y(0.7);
        let pose = Pose::from_scene(&Transform::from_rotation(turn));
        assert!(Quat::from_array(pose.rotation).abs_diff_eq(Quat::from_rotation_z(0.7), 1e-6));
    }

    fn add_goal(app: &mut App, key: &str, colors: Option<[String; 3]>) -> Entity {
        let mut control = goal_at([0.0, 0.0, 1.0]);
        if let Control::Transform { colors: c, .. } = &mut control {
            *c = colors;
        }
        send(
            app,
            ControlCommand::Add {
                key: key.into(),
                control,
            },
        );
        only::<TransformGizmo>(app).expect("a transform control")
    }

    fn red() -> Color {
        Srgba::hex("#ff0000").unwrap().into()
    }

    /// A control takes the host's colours when added, and changes them, and
    /// a transform control its axes, when configured; a bad colour changes
    /// nothing.
    #[test]
    fn a_control_takes_the_host_s_colours_and_axes() {
        let (mut app, body) = scene();
        robot(&mut app, body);
        let hex = |s: &str| s.to_string();
        let gizmo = add_goal(
            &mut app,
            "goal",
            Some([hex("#ff0000"), hex("#00ff00"), hex("#0000ff")]),
        );
        assert_eq!(
            app.world().get::<TransformGizmo>(gizmo).unwrap().colours[0],
            red()
        );
        send(
            &mut app,
            ControlCommand::Configure {
                key: "goal".into(),
                color: None,
                colors: Some([hex("#0000ff"), hex("not a colour"), hex("#ff0000")]),
                local: Some(true),
                face_viewer: None,
                opacity: None,
                interactive: None,
                size: None,
            },
        );
        let configured = app.world().get::<TransformGizmo>(gizmo).unwrap();
        assert_eq!(configured.colours[0], red(), "a bad colour changes none");
        assert_eq!(configured.space, GizmoSpace::Local);

        send(
            &mut app,
            ControlCommand::Add {
                key: TILT.into(),
                control: Control::Joint {
                    instance: ROBOT.into(),
                    joint: TILT.into(),
                    color: Some(hex("#ff0000")),
                    size: None,
                },
            },
        );
        let joint = only::<JointGizmo>(&mut app).unwrap();
        assert_eq!(app.world().get::<JointGizmo>(joint).unwrap().colour, red());
        send(
            &mut app,
            ControlCommand::Configure {
                key: TILT.into(),
                color: Some(hex("#00ff00")),
                colors: None,
                local: None,
                face_viewer: None,
                opacity: None,
                interactive: None,
                size: None,
            },
        );
        assert_eq!(
            app.world().get::<JointGizmo>(joint).unwrap().colour,
            Srgba::hex("#00ff00").unwrap().into()
        );
    }

    /// A transform control keeps its arrows on each axis's positive side
    /// unless added or configured to face the viewer; a configure that does
    /// not say leaves it as it is.
    #[test]
    fn a_transform_control_faces_the_viewer_only_when_asked() {
        let (mut app, body) = scene();
        robot(&mut app, body);
        let gizmo = add_goal(&mut app, "goal", None);
        let facing = |app: &App| {
            app.world()
                .get::<TransformGizmo>(gizmo)
                .unwrap()
                .face_viewer
        };
        assert!(!facing(&app), "positive by default");
        let set = |app: &mut App, face_viewer| {
            send(
                app,
                ControlCommand::Configure {
                    key: "goal".into(),
                    color: None,
                    colors: None,
                    local: None,
                    face_viewer,
                    opacity: None,
                    interactive: None,
                    size: None,
                },
            );
        };
        set(&mut app, Some(true));
        assert!(facing(&app));
        set(&mut app, None);
        assert!(facing(&app), "unsaid, unchanged");
        set(&mut app, Some(false));
        assert!(!facing(&app));

        let mut control = goal_at([0.0, 0.0, 1.0]);
        if let Control::Transform { face_viewer, .. } = &mut control {
            *face_viewer = true;
        }
        send(
            &mut app,
            ControlCommand::Add {
                key: "facing".into(),
                control,
            },
        );
        let facing_one = app
            .world_mut()
            .query::<&TransformGizmo>()
            .iter(app.world())
            .find(|g| g.key == "facing")
            .expect("the second control")
            .face_viewer;
        assert!(facing_one, "added facing the viewer");
    }

    /// A highlight lights one handle, or every one, until it is turned off;
    /// on a joint control it shows the control in hand.
    #[test]
    fn a_highlight_lights_a_handle_or_the_whole_control() {
        let (mut app, body) = scene();
        robot(&mut app, body);
        let gizmo = add_goal(&mut app, "goal", None);
        let highlight = |app: &mut App, handle, on| {
            send(
                app,
                ControlCommand::Highlight {
                    key: "goal".into(),
                    handle,
                    on,
                },
            );
            app.world()
                .get::<TransformGizmo>(gizmo)
                .unwrap()
                .highlight
                .clone()
        };
        let ring = GizmoHandle::Ring(1);
        assert_eq!(highlight(&mut app, Some(ring), true), vec![ring]);
        assert_eq!(highlight(&mut app, Some(ring), true), vec![ring], "once");
        assert_eq!(highlight(&mut app, Some(ring), false), vec![]);
        assert_eq!(
            highlight(&mut app, None, true).len(),
            GizmoHandle::ALL.len()
        );
        assert_eq!(highlight(&mut app, None, false), vec![]);

        send(
            &mut app,
            ControlCommand::Add {
                key: TILT.into(),
                control: Control::Joint {
                    instance: ROBOT.into(),
                    joint: TILT.into(),
                    color: None,
                    size: None,
                },
            },
        );
        send(
            &mut app,
            ControlCommand::Highlight {
                key: TILT.into(),
                handle: None,
                on: true,
            },
        );
        let joint = only::<JointGizmo>(&mut app).unwrap();
        assert!(app.world().get::<JointGizmo>(joint).unwrap().show_in_hand);
    }

    /// A highlight or a change sent in the frame its control is added applies
    /// once the control exists, rather than being lost.
    #[test]
    fn a_change_sent_with_the_add_is_not_lost() {
        let (mut app, body) = scene();
        robot(&mut app, body);
        let mut control = goal_at([0.0, 0.0, 1.0]);
        if let Control::Transform { colors, .. } = &mut control {
            *colors = None;
        }
        app.world_mut().resource_mut::<ControlCommands>().0.extend([
            ControlCommand::Add {
                key: "goal".into(),
                control,
            },
            ControlCommand::Highlight {
                key: "goal".into(),
                handle: None,
                on: true,
            },
            ControlCommand::Configure {
                key: "goal".into(),
                color: None,
                colors: None,
                local: Some(true),
                face_viewer: None,
                opacity: None,
                interactive: None,
                size: None,
            },
        ]);
        app.update();
        app.update();
        let gizmo = only::<TransformGizmo>(&mut app).unwrap();
        let gizmo = app.world().get::<TransformGizmo>(gizmo).unwrap();
        assert_eq!(gizmo.highlight.len(), GizmoHandle::ALL.len());
        assert_eq!(gizmo.space, GizmoSpace::Local);
    }

    /// A highlight or a change for a key nothing will add is dropped, rather
    /// than waiting, and asking for a frame, for good; one for a key whose add
    /// is still coming waits for it.
    #[test]
    fn a_change_for_a_key_nothing_will_add_is_dropped() {
        let (mut app, body) = scene();
        robot(&mut app, body);
        app.update();
        let frames_asked = |app: &mut App| {
            let mut messages = app.world_mut().resource_mut::<Messages<RequestRedraw>>();
            let asked = messages.len();
            messages.clear();
            asked
        };
        frames_asked(&mut app);
        send(
            &mut app,
            ControlCommand::Highlight {
                key: "nobody".into(),
                handle: None,
                on: true,
            },
        );
        send(
            &mut app,
            ControlCommand::Configure {
                key: "nobody".into(),
                color: Some("#ff0000".into()),
                colors: None,
                local: None,
                face_viewer: None,
                opacity: None,
                interactive: None,
                size: None,
            },
        );
        send(
            &mut app,
            ControlCommand::SetPose {
                key: "nobody".into(),
                pose: Pose {
                    position: [0.0, 0.0, 1.0],
                    rotation: [0.0, 0.0, 0.0, 1.0],
                },
                scale: None,
            },
        );
        assert!(app.world().resource::<ControlCommands>().0.is_empty());
        // Read before another update: a frame request outlives only two.
        assert_eq!(
            frames_asked(&mut app),
            0,
            "a dropped change asks for no frame"
        );
        app.update();
        app.update();
        assert_eq!(frames_asked(&mut app), 0, "the scene can idle");

        // Before the robot, a highlight waits for the add it will follow.
        let (mut app, body) = scene();
        send(
            &mut app,
            ControlCommand::Highlight {
                key: TILT.into(),
                handle: None,
                on: true,
            },
        );
        assert_eq!(app.world().resource::<ControlCommands>().0.len(), 1);
        robot(&mut app, body);
        send(
            &mut app,
            ControlCommand::Add {
                key: TILT.into(),
                control: Control::Joint {
                    instance: ROBOT.into(),
                    joint: TILT.into(),
                    color: None,
                    size: None,
                },
            },
        );
        app.update();
        let joint = only::<JointGizmo>(&mut app).unwrap();
        assert!(app.world().get::<JointGizmo>(joint).unwrap().show_in_hand);
    }

    fn configure(key: &str, opacity: Option<f32>, interactive: Option<bool>) -> ControlCommand {
        ControlCommand::Configure {
            key: key.into(),
            color: None,
            colors: None,
            local: None,
            face_viewer: None,
            opacity,
            interactive,
            size: None,
        }
    }

    /// A control fades to the opacity its host asks for, over the fade's
    /// length rather than at once, and stays there; a frame that comes long
    /// after the last takes one step of it, not the whole fade.
    #[test]
    fn a_control_fades_to_the_opacity_asked_for() {
        use bevy::time::TimeUpdateStrategy;
        use std::time::Duration;

        let (mut app, body) = scene();
        robot(&mut app, body);
        let frame = FADE_SECS / 4.0;
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(
            frame,
        )));
        let gizmo = add_goal(&mut app, "goal", None);
        let opacity = |app: &App| app.world().get::<TransformGizmo>(gizmo).unwrap().opacity;
        assert_eq!(opacity(&app), 1.0);

        send(&mut app, configure("goal", Some(0.0), None));
        let first = opacity(&app);
        assert!(first < 1.0 && first > 0.0, "part of the way: {first}");
        // The scene draws only on change, so a fade asks for its own frames
        // while it runs, and none once it has arrived.
        let frames_asked = |app: &mut App| {
            let mut messages = app.world_mut().resource_mut::<Messages<RequestRedraw>>();
            let asked = messages.len();
            messages.clear();
            asked
        };
        frames_asked(&mut app);
        app.update();
        assert!(frames_asked(&mut app) > 0, "a frame asked for mid-fade");
        for _ in 0..8 {
            app.update();
        }
        assert_eq!(opacity(&app), 0.0, "and there in the end");
        frames_asked(&mut app);
        app.update();
        assert_eq!(frames_asked(&mut app), 0, "none once it has arrived");

        // A long quiet spell before the next change is one step, not all.
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(5)));
        send(&mut app, configure("goal", Some(1.0), None));
        let after_a_pause = opacity(&app);
        assert!(after_a_pause < 1.0, "{after_a_pause}");
    }

    /// A small change takes the whole fade, as a large one does, and a fade
    /// sent somewhere new part way starts over from where it had got to.
    #[test]
    fn every_fade_takes_the_fade_s_length() {
        use bevy::time::TimeUpdateStrategy;
        use std::time::Duration;

        let (mut app, body) = scene();
        robot(&mut app, body);
        // Under the thirtieth of a second a step is held to.
        let frame = FADE_SECS / 8.0;
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(
            frame,
        )));
        let gizmo = add_goal(&mut app, "goal", None);
        let opacity = |app: &App| app.world().get::<TransformGizmo>(gizmo).unwrap().opacity;
        let near = |app: &App, wanted: f32| {
            let now = opacity(app);
            assert!((now - wanted).abs() < 1e-4, "{now}, not {wanted}");
        };
        let frames = |app: &mut App, count: usize| {
            for _ in 0..count {
                app.update();
            }
        };

        send(&mut app, configure("goal", Some(0.9), None));
        near(&app, 0.9875);
        frames(&mut app, 3);
        near(&app, 0.95);
        frames(&mut app, 4);
        assert_eq!(opacity(&app), 0.9, "there at the fade's length");
        // Exactly there, though the steps on the way round.
        send(&mut app, configure("goal", Some(0.1), None));
        frames(&mut app, 7);
        assert_eq!(opacity(&app), 0.1, "exactly what was asked for");
        send(&mut app, configure("goal", Some(0.9), None));
        frames(&mut app, 7);
        assert_eq!(opacity(&app), 0.9);

        // Sent somewhere new half way, it starts over from where it had got
        // to: half way from 0.9 to 0.1 is 0.5, so a fade to 0.5 holds there.
        send(&mut app, configure("goal", Some(0.1), None));
        frames(&mut app, 3);
        near(&app, 0.5);
        send(&mut app, configure("goal", Some(0.5), None));
        near(&app, 0.5);

        // Asked again for where it is already going, it runs on.
        send(&mut app, configure("goal", Some(1.0), None));
        near(&app, 0.5625);
        send(&mut app, configure("goal", Some(1.0), None));
        near(&app, 0.625);
    }

    /// A control sent with a change in the same batch as its add first
    /// shows with it: at the opacity asked for, not faded down from full,
    /// and inert from its first frame.
    #[test]
    fn a_control_added_dimmed_and_inert_first_shows_so() {
        let (mut app, body) = scene();
        robot(&mut app, body);
        {
            let mut asked = app.world_mut().resource_mut::<ControlCommands>();
            asked.0.push(ControlCommand::Add {
                key: "goal".into(),
                control: goal_at([0.0, 0.0, 1.0]),
            });
            asked.0.push(ControlCommand::Add {
                key: TILT.into(),
                control: Control::Joint {
                    instance: ROBOT.into(),
                    joint: TILT.into(),
                    color: None,
                    size: None,
                },
            });
            for key in ["goal", TILT] {
                asked.0.push(configure(key, Some(0.25), Some(false)));
            }
        }
        app.update();
        let goal = only::<TransformGizmo>(&mut app).unwrap();
        let joint = only::<JointGizmo>(&mut app).unwrap();
        let goal = app.world().get::<TransformGizmo>(goal).unwrap();
        assert_eq!((goal.opacity, goal.interactive), (0.25, false));
        let joint = app.world().get::<JointGizmo>(joint).unwrap();
        assert_eq!((joint.opacity, joint.interactive), (0.25, false));

        // Once shown, a change fades as ever.
        send(&mut app, configure("goal", Some(1.0), None));
        let goal = only::<TransformGizmo>(&mut app).unwrap();
        let now = app.world().get::<TransformGizmo>(goal).unwrap().opacity;
        assert!(now > 0.25 && now < 1.0, "{now}");
    }

    /// The commands run before the gizmos' own systems, so a control made
    /// inert is inert to the drag handling of the same frame. Ambiguity is
    /// an error here, so the order has to be stated rather than happen to
    /// fall out.
    #[test]
    fn commands_run_before_the_gizmos_own_systems() {
        use bevy::ecs::schedule::{LogLevel, ScheduleBuildSettings};

        #[derive(Resource, Default)]
        struct Seen(Vec<bool>);

        let (mut app, body) = scene();
        robot(&mut app, body);
        add_goal(&mut app, "goal", None);
        app.init_resource::<Seen>().add_systems(
            Update,
            (|gizmos: Query<&TransformGizmo>, mut seen: ResMut<Seen>| {
                seen.0.extend(gizmos.iter().map(|gizmo| gizmo.interactive));
            })
            .in_set(TransformGizmoSystems),
        );
        app.edit_schedule(Update, |schedule| {
            schedule.set_build_settings(ScheduleBuildSettings {
                ambiguity_detection: LogLevel::Error,
                ..default()
            });
        });
        app.update();
        assert_eq!(app.world().resource::<Seen>().0, [true]);
        send(&mut app, configure("goal", None, Some(false)));
        assert_eq!(app.world().resource::<Seen>().0, [true, false]);
    }

    /// Hidden part way through a fade and then given another opacity, a
    /// control takes it at once, and the fade it was on goes.
    #[test]
    fn a_control_hidden_part_way_through_a_fade_takes_a_new_opacity_at_once() {
        use bevy::time::TimeUpdateStrategy;
        use std::time::Duration;

        let (mut app, body) = scene();
        robot(&mut app, body);
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(
            FADE_SECS / 8.0,
        )));
        let gizmo = add_goal(&mut app, "goal", None);
        let opacity = |app: &App| app.world().get::<TransformGizmo>(gizmo).unwrap().opacity;
        send(&mut app, configure("goal", Some(0.5), None));
        let part_way = opacity(&app);
        assert!(part_way > 0.5 && part_way < 1.0, "{part_way}");
        send(
            &mut app,
            ControlCommand::Show {
                key: "goal".into(),
                visible: false,
            },
        );
        send(&mut app, configure("goal", Some(0.25), None));
        for _ in 0..8 {
            assert_eq!(opacity(&app), 0.25);
            app.update();
        }
    }

    /// Hidden and dimmed in one batch, a control takes the opacity at once,
    /// as it would a frame later.
    #[test]
    fn a_control_hidden_and_dimmed_together_takes_the_opacity_at_once() {
        let (mut app, body) = scene();
        robot(&mut app, body);
        let gizmo = add_goal(&mut app, "goal", None);
        {
            let mut asked = app.world_mut().resource_mut::<ControlCommands>();
            asked.0.push(ControlCommand::Show {
                key: "goal".into(),
                visible: false,
            });
            asked.0.push(configure("goal", Some(0.25), None));
        }
        app.update();
        let opacity = app.world().get::<TransformGizmo>(gizmo).unwrap().opacity;
        assert_eq!(opacity, 0.25);
    }

    /// A control added to a ghosted robot is hidden from its first frame, so
    /// an opacity sent the frame after takes hold at once.
    #[test]
    fn a_control_added_on_a_ghost_takes_an_opacity_at_once() {
        let (mut app, body) = scene();
        app.add_plugins(crate::hide_ghosted_controls);
        robot(&mut app, body);
        app.world_mut()
            .entity_mut(body)
            .insert(arora_visuals::ghost::Ghost);
        send(
            &mut app,
            ControlCommand::Add {
                key: TILT.into(),
                control: Control::Joint {
                    instance: ROBOT.into(),
                    joint: TILT.into(),
                    color: None,
                    size: None,
                },
            },
        );
        let joint = only::<JointGizmo>(&mut app).unwrap();
        assert_eq!(
            app.world().get::<Visibility>(joint),
            Some(&Visibility::Hidden),
            "hidden in the frame it is added"
        );
        send(&mut app, configure(TILT, Some(0.25), None));
        assert_eq!(app.world().get::<JointGizmo>(joint).unwrap().opacity, 0.25);
    }

    /// A joint control fades as a transform control does.
    #[test]
    fn a_joint_control_fades() {
        use bevy::time::TimeUpdateStrategy;
        use std::time::Duration;

        let (mut app, body) = scene();
        robot(&mut app, body);
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(
            FADE_SECS / 8.0,
        )));
        send(
            &mut app,
            ControlCommand::Add {
                key: TILT.into(),
                control: Control::Joint {
                    instance: ROBOT.into(),
                    joint: TILT.into(),
                    color: None,
                    size: None,
                },
            },
        );
        let joint = only::<JointGizmo>(&mut app).unwrap();
        let opacity = |app: &App| app.world().get::<JointGizmo>(joint).unwrap().opacity;
        send(&mut app, configure(TILT, Some(0.5), None));
        let part_way = opacity(&app);
        assert!(part_way > 0.5 && part_way < 1.0, "{part_way}");
        for _ in 0..7 {
            app.update();
        }
        assert_eq!(opacity(&app), 0.5);
    }

    /// A robot that arrives through commands before the passes, as one the
    /// mount indexes does, leaves both passes to a control waiting on it: a
    /// change sent with its add holds from the control's first frame.
    #[test]
    fn a_control_waiting_on_its_robot_first_shows_as_asked() {
        #[derive(Resource)]
        struct Arriving(Option<Entity>);

        let (mut app, body) = scene();
        {
            let mut asked = app.world_mut().resource_mut::<ControlCommands>();
            asked.0.push(ControlCommand::Add {
                key: "whole".into(),
                control: on_robot(),
            });
            asked.0.push(configure("whole", Some(0.25), Some(false)));
        }
        app.update();
        assert!(only::<TransformGizmo>(&mut app).is_none(), "no robot yet");

        app.insert_resource(Arriving(Some(body)));
        app.add_systems(
            Update,
            (|mut arriving: ResMut<Arriving>, mut commands: Commands| {
                if let Some(body) = arriving.0.take() {
                    commands.queue(move |world: &mut World| spawn_robot(world, body));
                }
            })
            .before(ControlSystems::Commands),
        );
        app.update();
        let gizmo = only::<TransformGizmo>(&mut app).expect("added with the robot");
        let gizmo = app.world().get::<TransformGizmo>(gizmo).unwrap();
        assert_eq!((gizmo.opacity, gizmo.interactive), (0.25, false));
    }

    /// A control dimmed while hidden takes the opacity at once, so it first
    /// shows at it, not fading down from where it was.
    #[test]
    fn a_control_dimmed_while_hidden_shows_at_it() {
        let (mut app, body) = scene();
        robot(&mut app, body);
        {
            let mut asked = app.world_mut().resource_mut::<ControlCommands>();
            asked.0.push(ControlCommand::Add {
                key: "goal".into(),
                control: goal_at([0.0, 0.0, 1.0]),
            });
            asked.0.push(ControlCommand::Show {
                key: "goal".into(),
                visible: false,
            });
        }
        app.update();
        app.update();
        let gizmo = only::<TransformGizmo>(&mut app).unwrap();
        {
            let mut asked = app.world_mut().resource_mut::<ControlCommands>();
            asked.0.push(configure("goal", Some(0.25), None));
            asked.0.push(ControlCommand::Show {
                key: "goal".into(),
                visible: true,
            });
        }
        app.update();
        let opacity = app.world().get::<TransformGizmo>(gizmo).unwrap().opacity;
        assert_eq!(opacity, 0.25);
    }

    /// Hidden by a ghosting rather than by its host, a control takes an
    /// opacity at once as well.
    #[test]
    fn a_control_hidden_by_a_ghosting_takes_an_opacity_at_once() {
        let (mut app, body) = scene();
        robot(&mut app, body);
        send(
            &mut app,
            ControlCommand::Add {
                key: "whole".into(),
                control: on_robot(),
            },
        );
        let gizmo = only::<TransformGizmo>(&mut app).unwrap();
        let root = only::<EntityInstance>(&mut app).unwrap();
        app.world_mut()
            .entity_mut(root)
            .insert(arora_visuals::ghost::Ghost);
        app.update();
        assert_eq!(
            app.world().get::<Visibility>(gizmo),
            Some(&Visibility::Hidden),
            "hidden with its robot"
        );
        send(&mut app, configure("whole", Some(0.25), None));
        let opacity = app.world().get::<TransformGizmo>(gizmo).unwrap().opacity;
        assert_eq!(opacity, 0.25);
    }

    /// An opacity outside 0 to 1 is held to it, rather than reaching the
    /// gizmo's materials as an alpha they cannot draw.
    #[test]
    fn an_opacity_out_of_range_is_held_to_it() {
        use bevy::time::TimeUpdateStrategy;
        use std::time::Duration;

        let (mut app, body) = scene();
        robot(&mut app, body);
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(
            1.0 / 60.0,
        )));
        let gizmo = add_goal(&mut app, "goal", None);
        let opacity = |app: &App| app.world().get::<TransformGizmo>(gizmo).unwrap().opacity;
        let settle = |app: &mut App| {
            for _ in 0..60 {
                app.update();
            }
        };
        send(&mut app, configure("goal", Some(-3.0), None));
        settle(&mut app);
        assert_eq!(opacity(&app), 0.0);
        send(&mut app, configure("goal", Some(7.0), None));
        settle(&mut app);
        assert_eq!(opacity(&app), 1.0);
    }

    /// `interactive` reaches either kind of control, and one command can
    /// carry it for both without a complaint about the other's settings.
    #[test]
    fn a_control_can_be_made_inert_and_live_again() {
        let (mut app, body) = scene();
        robot(&mut app, body);
        let goal = add_goal(&mut app, "goal", None);
        send(
            &mut app,
            ControlCommand::Add {
                key: TILT.into(),
                control: Control::Joint {
                    instance: ROBOT.into(),
                    joint: TILT.into(),
                    color: None,
                    size: None,
                },
            },
        );
        let joint = only::<JointGizmo>(&mut app).unwrap();
        for key in ["goal", TILT] {
            send(&mut app, configure(key, None, Some(false)));
        }
        assert!(!app.world().get::<TransformGizmo>(goal).unwrap().interactive);
        assert!(!app.world().get::<JointGizmo>(joint).unwrap().interactive);
        send(&mut app, configure("goal", None, Some(true)));
        assert!(app.world().get::<TransformGizmo>(goal).unwrap().interactive);
    }

    /// The JSON a host sends for a fade and an inert control reads as meant.
    #[test]
    fn opacity_and_interactive_read_from_json() {
        let command: ControlCommand = serde_json::from_value(serde_json::json!({
            "op": "configure", "key": "k", "opacity": 0.3, "interactive": false
        }))
        .unwrap();
        assert!(matches!(
            command,
            ControlCommand::Configure {
                opacity: Some(o),
                interactive: Some(false),
                ..
            } if o == 0.3
        ));
    }

    /// A host's press on a control's handle is reported by the control's
    /// key, with where it was.
    #[test]
    fn a_press_with_another_button_is_reported_by_key() {
        let (mut app, body) = scene();
        robot(&mut app, body);
        let gizmo = add_goal(&mut app, "goal", None);
        app.world_mut().trigger(GizmoPress {
            entity: gizmo,
            handle: GizmoHandle::Arrow(2),
            button: PointerButton::Secondary,
            position: Vec2::new(40.0, 30.0),
        });
        let presses = std::mem::take(&mut app.world_mut().resource_mut::<ControlPresses>().0);
        assert_eq!(presses.len(), 1);
        assert_eq!(
            (
                presses[0].key.as_str(),
                presses[0].handle,
                presses[0].button,
                presses[0].position
            ),
            (
                "goal",
                GizmoHandle::Arrow(2),
                PressButton::Secondary,
                [40.0, 30.0]
            )
        );
        let json = serde_json::to_value(&presses[0]).unwrap();
        assert_eq!(json["handle"], serde_json::json!({ "Arrow": 2 }));
        assert_eq!(json["button"], "secondary");
    }

    /// The JSON a host sends reads as the commands it means.
    #[test]
    fn commands_read_from_the_host_s_json() {
        let read =
            |json: serde_json::Value| -> ControlCommand { serde_json::from_value(json).unwrap() };
        assert!(matches!(
            read(serde_json::json!({
                "op": "add",
                "key": "k",
                "control": { "kind": "joint", "instance": "arm", "joint": "tilt" },
            })),
            ControlCommand::Add {
                control: Control::Joint { color: None, .. },
                ..
            }
        ));
        assert!(matches!(
            read(
                serde_json::json!({ "op": "highlight", "key": "k", "handle": { "Ring": 0 }, "on": true })
            ),
            ControlCommand::Highlight {
                handle: Some(GizmoHandle::Ring(0)),
                on: true,
                ..
            }
        ));
        assert!(matches!(
            read(serde_json::json!({ "op": "highlight", "key": "k", "on": false })),
            ControlCommand::Highlight {
                handle: None,
                on: false,
                ..
            }
        ));
        assert!(matches!(
            read(serde_json::json!({ "op": "configure", "key": "k", "local": true })),
            ControlCommand::Configure {
                local: Some(true),
                color: None,
                colors: None,
                ..
            }
        ));
    }

    /// A control is the size its add gives it, and changes to the size a
    /// configure gives it; given none, it is the shared default for its kind.
    #[test]
    fn a_control_takes_the_size_it_is_given() {
        let (mut app, body) = scene();
        robot(&mut app, body);
        let read = |json: &str| read_command(json).unwrap();
        send(
            &mut app,
            read(
                r#"{"op":"add","key":"goal","control":{"kind":"transform","target":{"kind":"goal","pose":{"position":[0,0,1]}},"size":{"pixels":40}}}"#,
            ),
        );
        let gizmo = only::<TransformGizmo>(&mut app).expect("a transform control");
        let size = |app: &App| app.world().get::<TransformGizmo>(gizmo).unwrap().size;
        assert_eq!(size(&app), GizmoSize::Pixels(40.0));
        send(
            &mut app,
            read(r#"{"op":"configure","key":"goal","size":{"metres":0.5}}"#),
        );
        assert_eq!(size(&app), GizmoSize::World(0.5));

        send(
            &mut app,
            read(&format!(
                r#"{{"op":"add","key":"tilt","control":{{"kind":"joint","instance":"{ROBOT}","joint":"{TILT}","size":{{"pixels":30}}}}}}"#
            )),
        );
        let joint = only::<JointGizmo>(&mut app).expect("a joint control");
        assert_eq!(
            app.world().get::<JointGizmo>(joint).unwrap().size,
            GizmoSize::Pixels(30.0)
        );

        send(
            &mut app,
            ControlCommand::Add {
                key: "plain".into(),
                control: goal_at([1.0, 0.0, 1.0]),
            },
        );
        let plain = app
            .world_mut()
            .query::<&TransformGizmo>()
            .iter(app.world())
            .find(|gizmo| gizmo.key == "plain")
            .expect("the plain control")
            .size;
        assert_eq!(plain, arora_visuals::transform_gizmo::GIZMO_SIZE);
    }

    /// A whole robot as a target reads as `instance`, and as the older
    /// `robot`; any other target is read as sent.
    #[test]
    fn a_whole_robot_target_reads_by_either_name() {
        let whole = |kind: &str| {
            let json = format!(
                r#"{{"op":"add","key":"k","control":{{"kind":"transform","target":{{"kind":"{kind}","instance":"arm"}}}}}}"#
            );
            read_command(&json).unwrap()
        };
        for kind in ["instance", "robot"] {
            assert!(
                matches!(
                    whole(kind),
                    ControlCommand::Add {
                        control: Control::Transform {
                            target: Target::Instance { ref instance },
                            ..
                        },
                        ..
                    } if instance == "arm"
                ),
                "{kind}"
            );
        }
        assert!(matches!(
            read_command(r#"{"op":"add","key":"k","control":{"kind":"joint","instance":"robot","joint":"tilt"}}"#)
                .unwrap(),
            ControlCommand::Add {
                control: Control::Joint { ref instance, .. },
                ..
            } if instance == "robot"
        ));
        assert!(read_command(r#"{"op":"show","key":"k","visible":true}"#).is_ok());
        assert!(read_command("not json").is_err());
    }

    #[test]
    fn face_viewer_reads_from_the_host_s_json() {
        let read =
            |json: serde_json::Value| -> ControlCommand { serde_json::from_value(json).unwrap() };
        assert!(matches!(
            read(serde_json::json!({
                "op": "add",
                "key": "k",
                "control": {
                    "kind": "transform",
                    "target": { "kind": "goal", "pose": { "position": [0, 0, 0], "rotation": [0, 0, 0, 1] } },
                    "faceViewer": true,
                },
            })),
            ControlCommand::Add {
                control: Control::Transform {
                    face_viewer: true,
                    ..
                },
                ..
            }
        ));
        assert!(matches!(
            read(serde_json::json!({ "op": "configure", "key": "k", "faceViewer": false })),
            ControlCommand::Configure {
                face_viewer: Some(false),
                ..
            }
        ));
        assert!(matches!(
            read(serde_json::json!({ "op": "configure", "key": "k" })),
            ControlCommand::Configure {
                face_viewer: None,
                ..
            }
        ));
    }

    #[test]
    fn a_pose_survives_the_round_trip() {
        let pose = Pose {
            position: [0.4, -1.2, 0.9],
            rotation: Quat::from_euler(EulerRot::ZYX, 0.3, -0.2, 1.1).to_array(),
        };
        let back = Pose::from_scene(&pose.to_scene());
        assert!(Vec3::from(back.position).abs_diff_eq(Vec3::from(pose.position), 1e-5));
        assert!(Quat::from_array(back.rotation).abs_diff_eq(Quat::from_array(pose.rotation), 1e-5));
    }
}
