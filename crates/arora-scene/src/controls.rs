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
    DragPhase, GizmoDrag, GizmoHandle, GizmoPress, GizmoSpace, TransformGizmo, TransformModes,
};
use bevy::picking::pointer::PointerButton;
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
    Joint {
        /// A CSS hex colour; the joint's own when absent.
        #[serde(default)]
        color: Option<String>,
    },
    Transform {
        target: Target,
        /// Every mode when absent.
        #[serde(default)]
        modes: Option<Vec<Mode>>,
        /// Handles along the target's own axes rather than the world's.
        #[serde(default)]
        local: bool,
        /// The X, Y and Z handles' CSS hex colours; red, green and blue when
        /// absent.
        #[serde(default)]
        colors: Option<[String; 3]>,
    },
}

/// A CSS hex colour, or `None`, said in the log, for one that is not.
fn colour(hex: &str) -> Option<Color> {
    match Srgba::hex(hex) {
        Ok(colour) => Some(colour.into()),
        Err(_) => {
            log::warn!("{hex:?} is not a hex colour; ignored");
            None
        }
    }
}

/// Three CSS hex colours, or `None` if any is not one.
fn colours(hex: &[String; 3]) -> Option<[Color; 3]> {
    Some([colour(&hex[0])?, colour(&hex[1])?, colour(&hex[2])?])
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
    /// Changes how a control looks or works, in place: a joint control's
    /// `color`, a transform control's `colors` and whether it is `local`, and
    /// for either its `opacity` and whether it is `interactive`. What is
    /// absent stays as it is, and what the control's kind does not take is
    /// ignored, so one command can recolour controls of both kinds.
    ///
    /// `opacity`, from 0 to 1, is eased into over [`FADE_SECS`], so a host
    /// sends the value it wants once. A control that is not `interactive`
    /// takes no hover, press or drag, and the pointer reaches the robot and
    /// the view behind it; a host's highlight still shows on it.
    Configure {
        key: String,
        #[serde(default)]
        color: Option<String>,
        #[serde(default)]
        colors: Option<[String; 3]>,
        #[serde(default)]
        local: Option<bool>,
        #[serde(default)]
        opacity: Option<f32>,
        #[serde(default)]
        interactive: Option<bool>,
    },
    /// Draws a control as the one under the pointer is drawn, or stops: a
    /// host pointing at it from elsewhere. `handle` names one handle of a
    /// transform control, in the form the overlay gives it; the whole control
    /// when absent.
    Highlight {
        key: String,
        #[serde(default)]
        handle: Option<GizmoHandle>,
        on: bool,
    },
}

impl ControlCommand {
    fn key(&self) -> &str {
        match self {
            Self::Add { key, .. }
            | Self::Remove { key }
            | Self::Show { key, .. }
            | Self::SetPose { key, .. }
            | Self::Configure { key, .. }
            | Self::Highlight { key, .. } => key,
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

/// The button of a press on a control that is the host's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PressButton {
    Secondary,
    Middle,
}

/// A press with a button other than the primary on a control, which is the
/// host's to act on (to open a menu for it, say). It moves nothing, and turns
/// and pans no view.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlPress {
    pub key: String,
    /// The handle pressed, in the form the overlay gives it.
    pub handle: GizmoHandle,
    pub button: PressButton,
    /// Where the press was, in the canvas's CSS pixels.
    pub position: [f32; 2],
}

/// Presses since a host last drained them.
#[derive(Resource, Default)]
pub struct ControlPresses(pub Vec<ControlPress>);

/// The controls in the scene, by key.
#[derive(Resource, Default)]
pub struct Controls(HashMap<String, Placed>);

struct Placed {
    gizmo: Entity,
    /// What a transform control moves; `None` for a joint control.
    target: Option<Entity>,
    /// A goal the control brought with it, which goes when it goes.
    goal: Option<Entity>,
    /// The opacity the host asked for, which the gizmo's eases toward.
    opacity: f32,
}

/// How long a control takes to fade to the opacity a host asks for.
pub const FADE_SECS: f32 = 0.2;

pub struct ControlsPlugin;

impl Plugin for ControlsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ControlCommands>()
            .init_resource::<ControlEvents>()
            .init_resource::<ControlPresses>()
            .init_resource::<Controls>()
            .add_systems(Update, (carry_out, fade).chain())
            .add_observer(report_transform_drag)
            .add_observer(report_joint_drag)
            .add_observer(report_press);
    }
}

/// Eases each control's gizmo toward the opacity its host asked for, over
/// [`FADE_SECS`], asking for frames while any is still on its way.
///
/// A step is at most a thirtieth of a second's: the scene draws only on
/// change, so the first frame after a quiet spell can be seconds after the
/// one before, which would otherwise take the whole fade in one jump.
fn fade(
    time: Res<Time>,
    controls: Res<Controls>,
    mut transform_gizmos: Query<&mut TransformGizmo>,
    mut joint_gizmos: Query<&mut JointGizmo>,
    mut redraw: MessageWriter<RequestRedraw>,
) {
    let step = time.delta_secs().min(1.0 / 30.0) / FADE_SECS;
    let toward = |now: f32, wanted: f32| now + (wanted - now).clamp(-step, step);
    let mut fading = false;
    for placed in controls.0.values() {
        let opacity = if let Ok(mut gizmo) = transform_gizmos.get_mut(placed.gizmo) {
            if gizmo.opacity == placed.opacity {
                continue;
            }
            gizmo.opacity = toward(gizmo.opacity, placed.opacity);
            gizmo.opacity
        } else if let Ok(mut gizmo) = joint_gizmos.get_mut(placed.gizmo) {
            if gizmo.opacity == placed.opacity {
                continue;
            }
            gizmo.opacity = toward(gizmo.opacity, placed.opacity);
            gizmo.opacity
        } else {
            continue;
        };
        fading |= opacity != placed.opacity;
    }
    if fading {
        redraw.write(RequestRedraw);
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
    mut transform_gizmos: Query<&mut TransformGizmo>,
    mut joint_gizmos: Query<&mut JointGizmo>,
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
    let asked = std::mem::take(&mut pending.0);
    // Keys an add is coming for, in this batch or waiting from an earlier one.
    let adding: std::collections::HashSet<String> = asked
        .iter()
        .filter_map(|command| match command {
            ControlCommand::Add { key, .. } => Some(key.clone()),
            _ => None,
        })
        .collect();
    // Whether a change for `key` should wait: for its control's gizmo, which
    // an add this frame spawns at the end of it, or for an add still to come.
    // Anything else is for a key nothing will add, such as one whose control
    // went with a swapped robot, and waiting would hold it, and the frames it
    // asks for, for good.
    let worth_waiting = |key: &str, controls: &Controls| {
        controls.0.contains_key(key) || robot.is_none() || adding.contains(key)
    };
    let mut waiting = Vec::new();
    // Whether to draw another frame: for a command that changed something, or
    // one waiting on a gizmo spawned at the end of this one.
    let mut again = false;
    for command in asked {
        again |= !matches!(
            command,
            ControlCommand::Configure { .. } | ControlCommand::Highlight { .. }
        );
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
                    Control::Joint { color } => {
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
                            color
                                .as_deref()
                                .and_then(colour)
                                .unwrap_or_else(|| joint.color.into()),
                        );
                        gizmo.limited = joint.is_limited();
                        gizmo.apply = false;
                        gizmo.key = key.clone();
                        Placed {
                            gizmo: commands.spawn(gizmo).id(),
                            target: None,
                            goal: None,
                            opacity: 1.0,
                        }
                    }
                    Control::Transform {
                        target,
                        modes,
                        local,
                        colors,
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
                        let base = TransformGizmo::new(target, camera);
                        let gizmo = commands
                            .spawn(TransformGizmo {
                                key: key.clone(),
                                colours: colors.as_ref().and_then(colours).unwrap_or(base.colours),
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
                                ..base
                            })
                            .id();
                        Placed {
                            gizmo,
                            target: Some(target),
                            goal,
                            opacity: 1.0,
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
            ControlCommand::Configure {
                key,
                color,
                colors,
                local,
                opacity,
                interactive,
            } => {
                // A control added this frame is spawned at the end of it, so
                // its gizmo is not there yet; the change waits a frame for
                // it, as one made before the add would.
                let gizmo = controls.0.get(&key).map(|placed| placed.gizmo);
                let spawned = gizmo.is_some_and(|gizmo| {
                    transform_gizmos.contains(gizmo) || joint_gizmos.contains(gizmo)
                });
                let (Some(gizmo), true) = (gizmo, spawned) else {
                    if worth_waiting(&key, &controls) {
                        again |= controls.0.contains_key(&key);
                        waiting.push(ControlCommand::Configure {
                            key,
                            color,
                            colors,
                            local,
                            opacity,
                            interactive,
                        });
                    } else {
                        log::debug!("no control {key:?} to configure");
                    }
                    continue;
                };
                again = true;
                if let (Some(opacity), Some(placed)) = (opacity, controls.0.get_mut(&key)) {
                    placed.opacity = opacity.clamp(0.0, 1.0);
                }
                // Neither kind's own: one command may carry these for both.
                let common = opacity.is_some() || interactive.is_some();
                if let Ok(mut gizmo) = transform_gizmos.get_mut(gizmo) {
                    if let Some(interactive) = interactive {
                        gizmo.interactive = interactive;
                    }
                    if let Some(colours) = colors.as_ref().and_then(colours) {
                        gizmo.colours = colours;
                    }
                    if let Some(local) = local {
                        gizmo.space = if local {
                            GizmoSpace::Local
                        } else {
                            GizmoSpace::World
                        };
                    }
                    if color.is_some() && colors.is_none() && local.is_none() && !common {
                        log::warn!("{key:?} is a transform control; give it colors");
                    }
                } else if let Ok(mut gizmo) = joint_gizmos.get_mut(gizmo) {
                    if let Some(interactive) = interactive {
                        gizmo.interactive = interactive;
                    }
                    if let Some(colour) = color.as_deref().and_then(colour) {
                        gizmo.colour = colour;
                    }
                    if color.is_none() && (colors.is_some() || local.is_some()) && !common {
                        log::warn!("{key:?} is a joint control; give it a color");
                    }
                }
            }
            ControlCommand::Highlight { key, handle, on } => {
                // Waits for a control added this frame, as a configure does.
                let gizmo = controls.0.get(&key).map(|placed| placed.gizmo);
                let spawned = gizmo.is_some_and(|gizmo| {
                    transform_gizmos.contains(gizmo) || joint_gizmos.contains(gizmo)
                });
                let (Some(gizmo), true) = (gizmo, spawned) else {
                    if worth_waiting(&key, &controls) {
                        again |= controls.0.contains_key(&key);
                        waiting.push(ControlCommand::Highlight { key, handle, on });
                    } else {
                        log::debug!("no control {key:?} to highlight");
                    }
                    continue;
                };
                again = true;
                if let Ok(mut gizmo) = transform_gizmos.get_mut(gizmo) {
                    match (handle, on) {
                        (Some(handle), true) => {
                            if !gizmo.highlight.contains(&handle) {
                                gizmo.highlight.push(handle);
                            }
                        }
                        (Some(handle), false) => gizmo.highlight.retain(|h| *h != handle),
                        (None, true) => gizmo.highlight = GizmoHandle::ALL.to_vec(),
                        (None, false) => gizmo.highlight.clear(),
                    }
                } else if let Ok(mut gizmo) = joint_gizmos.get_mut(gizmo) {
                    gizmo.show_in_hand = on;
                }
            }
        }
    }
    pending.0 = waiting;
    if again {
        redraw.write(RequestRedraw);
    }
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

fn report_press(
    press: On<GizmoPress>,
    transform_gizmos: Query<&TransformGizmo>,
    joint_gizmos: Query<&JointGizmo>,
    mut presses: ResMut<ControlPresses>,
) {
    let key = match (
        transform_gizmos.get(press.entity),
        joint_gizmos.get(press.entity),
    ) {
        (Ok(gizmo), _) => &gizmo.key,
        (_, Ok(gizmo)) => &gizmo.key,
        _ => return,
    };
    let button = match press.button {
        PointerButton::Secondary => PressButton::Secondary,
        PointerButton::Middle => PressButton::Middle,
        PointerButton::Primary => return,
    };
    if key.is_empty() {
        return;
    }
    presses.0.push(ControlPress {
        key: key.clone(),
        handle: press.handle,
        button,
        position: press.position.to_array(),
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
                control: Control::Joint { color: None },
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
                control: Control::Joint { color: None },
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
                control: Control::Joint { color: None },
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
                    colors: None,
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
            colors: None,
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
                control: Control::Joint { color: None },
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
                control: Control::Joint { color: None },
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
                opacity: None,
                interactive: None,
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
                    color: Some(hex("#ff0000")),
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
                opacity: None,
                interactive: None,
            },
        );
        assert_eq!(
            app.world().get::<JointGizmo>(joint).unwrap().colour,
            Srgba::hex("#00ff00").unwrap().into()
        );
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
                control: Control::Joint { color: None },
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
                opacity: None,
                interactive: None,
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
                opacity: None,
                interactive: None,
            },
        );
        assert!(app.world().resource::<ControlCommands>().0.is_empty());
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
                control: Control::Joint { color: None },
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
            opacity,
            interactive,
        }
    }

    /// A control eases to the opacity its host asks for, over the fade's
    /// length rather than at once, and stays there; a frame that comes long
    /// after the last takes one step of it, not the whole fade.
    #[test]
    fn a_control_eases_to_the_opacity_asked_for() {
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
        for _ in 0..8 {
            app.update();
        }
        assert_eq!(opacity(&app), 0.0, "and there in the end");

        // A long quiet spell before the next change is one step, not all.
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(5)));
        send(&mut app, configure("goal", Some(1.0), None));
        let after_a_pause = opacity(&app);
        assert!(after_a_pause < 1.0, "{after_a_pause}");
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
                control: Control::Joint { color: None },
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
            read(serde_json::json!({ "op": "add", "key": "k", "control": { "kind": "joint" } })),
            ControlCommand::Add {
                control: Control::Joint { color: None },
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
