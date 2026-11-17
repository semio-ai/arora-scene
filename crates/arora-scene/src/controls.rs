//! Controls a driver adds, shows, hides, moves and removes, by keys of its own.
//!
//! The scene does not decide what a control means. A driver (a device, an IK
//! solver, an editor) adds a control under a key, hears each drag on it as a
//! [`ControlEvent`], and writes back whatever the drag should change. **A drag
//! moves nothing by itself**: a joint control reports the value the drag asks
//! for, and a transform control reports the pose, and the robot moves when the
//! driver writes that value or pose. One path changes the scene, and it is the
//! one every other writer uses, so the data a driver keeps and what the scene
//! shows cannot disagree.
//!
//! - A **joint control** is keyed by its joint's value animatable, the key the
//!   driver writes the joint under.
//! - A **transform control** moves the robot, one of its elements, or a goal:
//!   a pose with nothing drawn but the control itself, which is what an IK
//!   target is.
//!
//! **Poses are in the driver's frame: Z up, metres, quaternions as
//! `[x, y, z, w]`.** Bevy is Y up; [`Pose`] converts at this boundary, so a
//! driver never sees the scene's own axes.

use std::collections::HashMap;
use std::f32::consts::FRAC_PI_2;

use arora_visuals::instance::EntityInstance;
use arora_visuals::joint_gizmo::{JointDrag, JointGizmo};
use arora_visuals::transform_gizmo::{
    DragPhase, GizmoDrag, GizmoSpace, TransformGizmo, TransformModes,
};
use bevy::prelude::*;
use bevy::window::RequestRedraw;
use serde::{Deserialize, Serialize};

use crate::joints::{JointRig, RobotJoints};
use crate::OrbitCamera;

/// A pose as a driver sees it: Z up, in metres, with the rotation as a
/// quaternion `[x, y, z, w]`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pose {
    pub position: [f32; 3],
    #[serde(default = "no_turn")]
    pub rotation: [f32; 4],
}

fn no_turn() -> [f32; 4] {
    [0.0, 0.0, 0.0, 1.0]
}

/// The turn that carries the scene's Y-up axes onto the driver's Z-up ones:
/// a quarter turn about X takes Y to Z and Z to -Y.
fn to_driver() -> Quat {
    Quat::from_rotation_x(FRAC_PI_2)
}

impl Pose {
    /// A pose in the scene's world, as the driver sees it.
    pub fn from_scene(transform: &Transform) -> Self {
        let c = to_driver();
        Self {
            position: (c * transform.translation).to_array(),
            rotation: (c * transform.rotation * c.inverse()).to_array(),
        }
    }

    /// This pose in the scene's world.
    pub fn to_scene(&self) -> Transform {
        let c = to_driver().inverse();
        Transform {
            translation: c * Vec3::from(self.position),
            rotation: c * Quat::from_array(self.rotation).normalize() * c.inverse(),
            scale: Vec3::ONE,
        }
    }
}

/// What a transform control moves.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Target {
    /// The robot as a whole.
    Robot,
    /// One element, by the id `RobotData` gives it.
    Element { id: String },
    /// A goal standing at `pose`, with nothing drawn but the control.
    Goal { pose: Pose },
}

/// What a transform control's handles can do.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    Translate,
    Rotate,
    Scale,
}

/// A control to add.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Control {
    /// A ring or bar on the joint whose value animatable is the control's key.
    Joint,
    Transform {
        target: Target,
        /// Every mode when absent.
        #[serde(default)]
        modes: Option<Vec<Mode>>,
        /// Handles along the target's own axes rather than the world's.
        #[serde(default)]
        local: bool,
    },
}

/// A driver's instruction about one control.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum ControlCommand {
    Add {
        key: String,
        control: Control,
    },
    Remove {
        key: String,
    },
    Show {
        key: String,
        visible: bool,
    },
    /// Moves a transform control's target to `pose`, in the world, and, given
    /// `scale`, stretches it along its own axes: what a driver writes back
    /// after a drag on the move, turn or scale handles.
    SetPose {
        key: String,
        pose: Pose,
        #[serde(default)]
        scale: Option<[f32; 3]>,
    },
}

impl ControlCommand {
    fn key(&self) -> &str {
        match self {
            Self::Add { key, .. }
            | Self::Remove { key }
            | Self::Show { key, .. }
            | Self::SetPose { key, .. } => key,
        }
    }
}

/// On a control the driver has hidden. The scene also hides controls on its
/// own account (on a ghosted robot, say), and it shows a control again only
/// when neither wants it hidden.
#[derive(Component)]
pub struct HiddenByDriver;

/// Instructions waiting for the next frame. One that cannot be carried out
/// yet, because the robot has not spawned, waits for the frame that can.
#[derive(Resource, Default)]
pub struct ControlCommands(pub Vec<ControlCommand>);

/// Where a drag is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    Start,
    Move,
    End,
}

impl From<DragPhase> for Phase {
    fn from(phase: DragPhase) -> Self {
        match phase {
            DragPhase::Start => Self::Start,
            DragPhase::Move => Self::Move,
            DragPhase::End => Self::End,
        }
    }
}

/// A drag on a control: what it asks its target to become. Nothing has moved;
/// the driver writes the change if it wants it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlEvent {
    pub key: String,
    pub phase: Phase,
    /// A joint control's value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<f32>,
    /// A transform control's target's pose, in the world.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pose: Option<Pose>,
    /// A transform control's target's scale, along its own axes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<[f32; 3]>,
}

/// Drags since a host last drained them.
#[derive(Resource, Default)]
pub struct ControlEvents(pub Vec<ControlEvent>);

/// The controls in the scene, by key.
#[derive(Resource, Default)]
pub struct Controls(HashMap<String, Placed>);

struct Placed {
    gizmo: Entity,
    /// What a transform control moves; `None` for a joint control.
    target: Option<Entity>,
    /// A goal the control brought with it, which goes when it goes.
    goal: Option<Entity>,
}

pub struct ControlsPlugin;

impl Plugin for ControlsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ControlCommands>()
            .init_resource::<ControlEvents>()
            .init_resource::<Controls>()
            .add_systems(Update, carry_out)
            .add_observer(report_transform_drag)
            .add_observer(report_joint_drag);
    }
}

/// Either kind of gizmo a control spawns.
type AnyGizmo = Or<(With<TransformGizmo>, With<JointGizmo>)>;

#[allow(clippy::too_many_arguments)]
fn carry_out(
    mut pending: ResMut<ControlCommands>,
    mut controls: ResMut<Controls>,
    mut commands: Commands,
    cameras: Query<Entity, With<OrbitCamera>>,
    instances: Query<(Entity, &EntityInstance, Option<&JointRig>)>,
    joints: Option<Res<RobotJoints>>,
    parents: Query<&ChildOf>,
    globals: Query<&GlobalTransform>,
    mut transforms: Query<&mut Transform>,
    gizmos: Query<(), AnyGizmo>,
    mut redraw: MessageWriter<RequestRedraw>,
) {
    // A control whose gizmo is gone went with the robot it was on, when the
    // robot was swapped; its key is free again.
    controls.0.retain(|_, placed| gizmos.contains(placed.gizmo));
    if pending.0.is_empty() {
        return;
    }
    let camera = cameras.iter().next();
    let robot = instances.iter().next();
    let mut waiting = Vec::new();
    for command in std::mem::take(&mut pending.0) {
        match command {
            ControlCommand::Add { key, control } => {
                let (Some(camera), Some((root, instance, rig))) = (camera, robot) else {
                    waiting.push(ControlCommand::Add { key, control });
                    continue;
                };
                if let Some(old) = controls.0.remove(&key) {
                    despawn(&mut commands, old);
                }
                let placed = match control {
                    Control::Joint => {
                        let joint = joints
                            .as_ref()
                            .and_then(|j| j.0.iter().find(|j| j.animatable.as_ref() == Some(&key)));
                        let binding = rig
                            .and_then(|rig| rig.0.get(&key))
                            .and_then(|b| b.iter().find(|b| b.mimic.is_none()).or(b.first()));
                        let (Some(joint), Some(binding)) = (joint, binding) else {
                            log::warn!("no joint is driven by {key:?}; no control added");
                            continue;
                        };
                        let current = transforms
                            .get(binding.body)
                            .map(|t| binding.value_of(t))
                            .unwrap_or(binding.rest_value);
                        let mut gizmo = JointGizmo::new(
                            binding.body,
                            camera,
                            binding.motion,
                            binding.axis,
                            joint.min,
                            joint.max,
                            current,
                            joint.color.into(),
                        );
                        gizmo.limited = joint.is_limited();
                        gizmo.apply = false;
                        gizmo.key = key.clone();
                        Placed {
                            gizmo: commands.spawn(gizmo).id(),
                            target: None,
                            goal: None,
                        }
                    }
                    Control::Transform {
                        target,
                        modes,
                        local,
                    } => {
                        let (target, goal) = match target {
                            Target::Robot => (root, None),
                            Target::Element { id } => match instance.bindings.elements.get(&id) {
                                Some(element) => (element.node, None),
                                None => {
                                    log::warn!("no element {id:?}; no control added for {key:?}");
                                    continue;
                                }
                            },
                            Target::Goal { pose } => {
                                let goal = commands
                                    .spawn((pose.to_scene(), Visibility::default()))
                                    .id();
                                (goal, Some(goal))
                            }
                        };
                        let modes = match modes {
                            None => TransformModes::ALL,
                            Some(modes) => TransformModes {
                                translate: modes.iter().any(|m| matches!(m, Mode::Translate)),
                                rotate: modes.iter().any(|m| matches!(m, Mode::Rotate)),
                                scale: modes.iter().any(|m| matches!(m, Mode::Scale)),
                            },
                        };
                        let gizmo = commands
                            .spawn(TransformGizmo {
                                key: key.clone(),
                                modes,
                                space: if local {
                                    GizmoSpace::Local
                                } else {
                                    GizmoSpace::World
                                },
                                // Labelled for the driver's Z-up world, as
                                // the view gizmo is.
                                axes: TransformGizmo::Z_UP,
                                apply: false,
                                ..TransformGizmo::new(target, camera)
                            })
                            .id();
                        Placed {
                            gizmo,
                            target: Some(target),
                            goal,
                        }
                    }
                };
                controls.0.insert(key, placed);
            }
            ControlCommand::Remove { key } => {
                if let Some(old) = controls.0.remove(&key) {
                    despawn(&mut commands, old);
                }
                // Whatever the driver asked of this key that is still
                // waiting goes too, or an add made before the robot arrived
                // would bring back a control the driver has since removed.
                waiting.retain(|waiting| waiting.key() != key);
            }
            ControlCommand::Show { key, visible } => match controls.0.get(&key) {
                Some(placed) => {
                    let mut gizmo = commands.entity(placed.gizmo);
                    if visible {
                        gizmo
                            .remove::<HiddenByDriver>()
                            .insert(Visibility::Inherited);
                    } else {
                        gizmo.insert((HiddenByDriver, Visibility::Hidden));
                    }
                }
                // Shown or hidden before it was added: the add is waiting
                // too, so this waits behind it.
                None => waiting.push(ControlCommand::Show { key, visible }),
            },
            ControlCommand::SetPose { key, pose, scale } => {
                let target = controls.0.get(&key).and_then(|p| p.target);
                // A goal added this frame is spawned at the end of it, so it
                // has no transform yet; the pose waits a frame for it, as one
                // set before the add would.
                let Some((target, mut transform)) =
                    target.and_then(|t| transforms.get_mut(t).ok().map(|tr| (t, tr)))
                else {
                    if target.is_none() && controls.0.contains_key(&key) {
                        log::warn!("{key:?} is a joint control; write its value instead");
                    } else {
                        waiting.push(ControlCommand::SetPose { key, pose, scale });
                    }
                    continue;
                };
                let world = pose.to_scene();
                // The pose is in the world; the target's transform is in its
                // parent's frame.
                let local = match parents
                    .get(target)
                    .ok()
                    .and_then(|p| globals.get(p.parent()).ok())
                {
                    Some(parent) => parent.affine().inverse() * world.compute_affine(),
                    None => world.compute_affine(),
                };
                let (_, rotation, translation) = local.to_scale_rotation_translation();
                transform.translation = translation;
                transform.rotation = rotation;
                if let Some(scale) = scale {
                    transform.scale = Vec3::from(scale);
                }
            }
        }
    }
    pending.0 = waiting;
    redraw.write(RequestRedraw);
}

fn despawn(commands: &mut Commands, placed: Placed) {
    commands.entity(placed.gizmo).despawn();
    if let Some(goal) = placed.goal {
        commands.entity(goal).despawn();
    }
}

fn report_transform_drag(
    drag: On<GizmoDrag>,
    gizmos: Query<&TransformGizmo>,
    parents: Query<&ChildOf>,
    globals: Query<&GlobalTransform>,
    mut events: ResMut<ControlEvents>,
) {
    let Ok(gizmo) = gizmos.get(drag.entity) else {
        return;
    };
    if gizmo.key.is_empty() {
        return;
    }
    // The drag gives the target's transform in its parent's frame.
    let world = match parents
        .get(drag.target)
        .ok()
        .and_then(|p| globals.get(p.parent()).ok())
    {
        Some(parent) => parent.mul_transform(drag.transform).compute_transform(),
        None => drag.transform,
    };
    events.0.push(ControlEvent {
        key: gizmo.key.clone(),
        phase: drag.phase.into(),
        value: None,
        pose: Some(Pose::from_scene(&world)),
        scale: Some(drag.transform.scale.to_array()),
    });
}

fn report_joint_drag(
    drag: On<JointDrag>,
    gizmos: Query<&JointGizmo>,
    mut events: ResMut<ControlEvents>,
) {
    let Ok(gizmo) = gizmos.get(drag.entity) else {
        return;
    };
    if gizmo.key.is_empty() {
        return;
    }
    events.0.push(ControlEvent {
        key: gizmo.key.clone(),
        phase: drag.phase.into(),
        value: Some(drag.value),
        pose: None,
        scale: None,
    });
}

#[cfg(test)]
mod tests {
    use arora_visuals::binding::SceneBindings;
    use arora_visuals::transform_gizmo::GizmoHandle;

    use super::*;
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

    /// A scene with a camera and a body the tilt joint moves, but no robot
    /// instance yet: `robot` adds one.
    fn scene() -> (App, Entity) {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<RequestRedraw>()
            .add_plugins(ControlsPlugin)
            .insert_resource(RobotJoints(vec![tilt()]));
        app.world_mut().spawn(OrbitCamera);
        let body = app.world_mut().spawn(Transform::default()).id();
        (app, body)
    }

    fn robot(app: &mut App, body: Entity) {
        let camera = app
            .world_mut()
            .query_filtered::<Entity, With<OrbitCamera>>()
            .single(app.world())
            .unwrap();
        let mut rig = JointRig::default();
        rig.0.insert(
            TILT.into(),
            vec![JointBinding::new(&tilt(), body, Transform::default()).unwrap()],
        );
        app.world_mut().spawn((
            rig,
            EntityInstance {
                id: "robot-0".into(),
                asset_path: String::new(),
                meta: Default::default(),
                bindings: SceneBindings::default(),
                camera,
                draw_order: 0,
                ready: true,
            },
        ));
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
                control: Control::Joint,
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
                control: Control::Joint,
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
                control: Control::Joint,
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
        // Neither shown on a dead entity nor applied: it waits, as for a
        // control not yet added, until the driver adds the key again.
        assert_eq!(
            app.world().resource::<ControlCommands>().0.len(),
            1,
            "the show waits for an add"
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
                    target: Target::Goal { pose: at },
                    modes: None,
                    local: false,
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

    fn goal_at(position: [f32; 3]) -> Control {
        Control::Transform {
            target: Target::Goal {
                pose: Pose {
                    position,
                    rotation: no_turn(),
                },
            },
            modes: None,
            local: false,
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
                control: Control::Joint,
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
                control: Control::Joint,
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
