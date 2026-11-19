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
    DragPhase, GizmoDrag, GizmoHandle, GizmoPress, GizmoSpace, TransformGizmo,
    TransformGizmoSystems, TransformModes,
};
use bevy::picking::pointer::PointerButton;
use bevy::prelude::*;
use bevy::window::RequestRedraw;
use serde::{Deserialize, Serialize};

use crate::joints::JointRig;
use crate::robots::RobotAsset;
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
    /// A robot as a whole: its root, by the host's id for its instance.
    Robot { instance: String },
    /// One element of a robot, by the id `RobotData` gives it.
    Element { instance: String, id: String },
    /// A goal standing at `pose`, in the world, with nothing drawn but the
    /// control.
    Goal { pose: Pose },
}

impl Target {
    /// The instance the target is on, if it is on one.
    fn instance(&self) -> Option<&str> {
        match self {
            Target::Robot { instance } | Target::Element { instance, .. } => Some(instance),
            Target::Goal { .. } => None,
        }
    }
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
    /// A ring or bar on a robot's joint.
    Joint {
        /// The host's id for the robot's instance.
        instance: String,
        /// The joint, by the animatable that drives its value.
        joint: String,
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
    /// `opacity`, from 0 to 1, is faded to evenly over [`FADE_SECS`] (longer
    /// while frames come slower than thirty a second), so a host sends the
    /// value it wants once. It is how the control looks only: a faded control
    /// still takes the pointer. A control that is not `interactive` takes no hover,
    /// press or drag, the pointer reaches the robot and the view behind it,
    /// and a drag in hand ends; a host's highlight still shows on it.
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

impl Control {
    /// The instance the control is on, if it is on one.
    fn instance(&self) -> Option<&str> {
        match self {
            Control::Joint { instance, .. } => Some(instance),
            Control::Transform { target, .. } => target.instance(),
        }
    }
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
    /// The opacity the host asked for, which the gizmo's fades toward.
    opacity: f32,
    /// The fade toward `opacity` under way, if one is.
    fade: Option<Fade>,
    /// Whether its gizmo shows: drawn, and hidden neither by the host nor by
    /// its robot's being ghosted. While it does not, an opacity asked for is
    /// taken at once rather than faded to, so a control added dimmed, or
    /// dimmed while hidden, never shows at full strength on its way there.
    shown: bool,
}

/// A fade under way. It runs for [`FADE_SECS`] however far it has to go, so
/// it keeps where it started rather than moving at a set rate, which would
/// take a small change in a frame or two.
struct Fade {
    /// The gizmo's opacity when the fade began, read on its first frame.
    from: Option<f32>,
    /// How long it has run, in steps of at most a thirtieth of a second.
    elapsed: f32,
}

/// How long a control takes to fade to the opacity a host asks for.
pub const FADE_SECS: f32 = 0.2;

pub struct ControlsPlugin;

/// The controls' systems, for another system to order itself against: the
/// passes that carry out commands, then the fade.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum ControlSystems {
    Commands,
    Fade,
}

impl Plugin for ControlsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ControlCommands>()
            .init_resource::<ControlEvents>()
            .init_resource::<ControlPresses>()
            .init_resource::<Controls>()
            // Twice, the gizmos an add spawns being there for the second:
            // a change sent with its control's add then takes hold the
            // frame the control first shows, rather than one frame later,
            // live and at full strength in between. Both before the
            // gizmos' own drag handling, so a left press in the frame a
            // control is made inert does not take it. A press with another
            // button is reported in `PreUpdate`, before either pass, so one
            // in that frame can still reach the host: a press and a change
            // arriving between the same two frames have no order to keep.
            .add_systems(
                Update,
                (
                    carry_out.in_set(ControlSystems::Commands),
                    carry_out.in_set(ControlSystems::Commands),
                    fade.in_set(ControlSystems::Fade),
                )
                    .chain()
                    .before(TransformGizmoSystems),
            )
            .add_observer(report_transform_drag)
            .add_observer(report_joint_drag)
            .add_observer(report_press);
    }
}

/// Fades each control's gizmo evenly from where its fade began to the opacity
/// its host asked for, over [`FADE_SECS`], asking for frames while any fade
/// is still under way.
///
/// A step is at most a thirtieth of a second's: the scene draws only on
/// change, so the first frame after a quiet spell can be seconds after the
/// one before, which would otherwise take the whole fade in one jump.
fn fade(
    time: Res<Time>,
    mut controls: ResMut<Controls>,
    mut transform_gizmos: Query<&mut TransformGizmo>,
    mut joint_gizmos: Query<&mut JointGizmo>,
    visibility: Query<&Visibility>,
    mut redraw: MessageWriter<RequestRedraw>,
) {
    let step = time.delta_secs().min(1.0 / 30.0);
    let mut fading = false;
    for placed in controls.0.values_mut() {
        // Drawn after this, in the frame's own `PostUpdate`, unless hidden:
        // whatever hides a control, the host or a ghosting, does it here.
        placed.shown = visibility
            .get(placed.gizmo)
            .is_ok_and(|visibility| *visibility != Visibility::Hidden);
        let Some(fade) = placed.fade.as_mut() else {
            continue;
        };
        let opacity = if let Ok(gizmo) = transform_gizmos.get_mut(placed.gizmo) {
            gizmo.map_unchanged(|gizmo| &mut gizmo.opacity)
        } else if let Ok(gizmo) = joint_gizmos.get_mut(placed.gizmo) {
            gizmo.map_unchanged(|gizmo| &mut gizmo.opacity)
        } else {
            // Its gizmo is gone, and the fade with it.
            placed.fade = None;
            continue;
        };
        let from = *fade.from.get_or_insert(*opacity);
        fade.elapsed += step;
        let done = (fade.elapsed / FADE_SECS).min(1.0);
        let mut opacity = opacity;
        *opacity = from + (placed.opacity - from) * done;
        if done >= 1.0 {
            // Exactly what was asked for, whatever the rounding on the way.
            *opacity = placed.opacity;
            placed.fade = None;
        } else {
            fading = true;
        }
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
    instances: Query<(Entity, &EntityInstance, Option<&JointRig>, &RobotAsset)>,
    robots: Query<&RobotAsset>,
    incoming: Query<(), With<crate::robots::Incoming>>,
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
    // The robot showing in an instance, once it is indexed.
    let robot = |name: &str| {
        instances
            .iter()
            .find(|(_, instance, ..)| instance.id == name)
    };
    // Whether a robot is showing or loading in an instance: what a control on
    // it waits for.
    let coming = |name: &str| robots.iter().any(|robot| robot.instance == name);
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
    // an add spawns at the end of its pass, or for an add still to come, in
    // this batch or behind a robot still loading, as a change sent before
    // its add is. Anything else is for a key nothing will add, such as one
    // whose control went with a swapped robot, and waiting would hold it, and
    // the frames it asks for, for good.
    let loading = !incoming.is_empty();
    let worth_waiting = |key: &str, controls: &Controls| {
        controls.0.contains_key(key) || adding.contains(key) || loading
    };
    let mut waiting = Vec::new();
    // Whether to draw another frame: for a command that changed something, or
    // one waiting on a gizmo spawned at the end of this pass.
    let mut again = false;
    for command in asked {
        again |= !matches!(
            command,
            ControlCommand::Configure { .. }
                | ControlCommand::Highlight { .. }
                | ControlCommand::SetPose { .. }
        );
        match command {
            ControlCommand::Add { key, control } => {
                // A control on a robot waits for it while it is loading; one
                // on a robot nothing is loading is dropped.
                let on = control
                    .instance()
                    .map(|name| (name.to_string(), robot(name)));
                let robot = match (&on, camera) {
                    (Some((_, Some(robot))), Some(_)) => Some(*robot),
                    (None, Some(_)) => None,
                    (Some((name, None)), _) if !coming(name) => {
                        log::warn!("no robot {name:?}; no control added for {key:?}");
                        continue;
                    }
                    _ => {
                        waiting.push(ControlCommand::Add { key, control });
                        continue;
                    }
                };
                let Some(camera) = camera else {
                    continue;
                };
                if let Some(old) = controls.0.remove(&key) {
                    despawn(&mut commands, old);
                }
                let placed = match control {
                    Control::Joint { joint, color, .. } => {
                        let Some((_, _, rig, asset)) = robot else {
                            continue;
                        };
                        let declared = asset
                            .joints
                            .iter()
                            .find(|j| j.animatable.as_ref() == Some(&joint));
                        let binding = rig
                            .and_then(|rig| rig.0.get(&joint))
                            .and_then(|b| b.iter().find(|b| b.mimic.is_none()).or(b.first()));
                        let (Some(declared), Some(binding)) = (declared, binding) else {
                            log::warn!(
                                "no joint of {} is driven by {joint:?}; no control added for {key:?}",
                                asset.instance
                            );
                            continue;
                        };
                        let joint = declared;
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
                            fade: None,
                            shown: false,
                        }
                    }
                    Control::Transform {
                        target,
                        modes,
                        local,
                        colors,
                    } => {
                        let (target, goal) = match (target, robot) {
                            (Target::Robot { .. }, Some((root, ..))) => (root, None),
                            (Target::Element { id, .. }, Some((_, instance, ..))) => {
                                match instance.bindings.elements.get(&id) {
                                    Some(element) => (element.node, None),
                                    None => {
                                        log::warn!(
                                            "no element {id:?} on {}; no control added for {key:?}",
                                            instance.id
                                        );
                                        continue;
                                    }
                                }
                            }
                            (Target::Robot { .. } | Target::Element { .. }, None) => continue,
                            (Target::Goal { pose }, _) => {
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
                            fade: None,
                            shown: false,
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
            ControlCommand::Show { key, visible } => match controls.0.get_mut(&key) {
                Some(placed) => {
                    let mut gizmo = commands.entity(placed.gizmo);
                    if visible {
                        gizmo
                            .remove::<HiddenByDriver>()
                            .insert(Visibility::Inherited);
                    } else {
                        gizmo.insert((HiddenByDriver, Visibility::Hidden));
                        // Hidden from now, for a change later in the batch;
                        // the fade reads whether it shows afresh each frame.
                        placed.shown = false;
                    }
                }
                // Shown or hidden before it was added: the add is waiting
                // too, so this waits behind it.
                None => waiting.push(ControlCommand::Show { key, visible }),
            },
            ControlCommand::SetPose { key, pose, scale } => {
                let target = controls.0.get(&key).and_then(|p| p.target);
                // A goal added in this pass is spawned at the end of it, so it
                // has no transform yet; the pose waits for the second pass, as
                // one set before the add would.
                let Some((target, mut transform)) =
                    target.and_then(|t| transforms.get_mut(t).ok().map(|tr| (t, tr)))
                else {
                    if target.is_none() && controls.0.contains_key(&key) {
                        log::warn!("{key:?} is a joint control; write its value instead");
                    } else if worth_waiting(&key, &controls) {
                        again |= controls.0.contains_key(&key);
                        waiting.push(ControlCommand::SetPose { key, pose, scale });
                    } else {
                        log::debug!("no control {key:?} to pose");
                    }
                    continue;
                };
                again = true;
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
                // A control added in this pass is spawned at the end of it,
                // so its gizmo is not there yet; the change waits for the
                // second pass, as one made before the add would.
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
                // An opacity for a control not yet drawn, taken as it is.
                let mut at_once = None;
                if let (Some(opacity), Some(placed)) = (opacity, controls.0.get_mut(&key)) {
                    let opacity = opacity.clamp(0.0, 1.0);
                    if !placed.shown {
                        placed.opacity = opacity;
                        placed.fade = None;
                        at_once = Some(opacity);
                    } else if opacity != placed.opacity {
                        // Asked again for where it is already going, a fade
                        // runs on; asked for somewhere new, it starts over
                        // from where the gizmo has got to.
                        placed.opacity = opacity;
                        placed.fade = Some(Fade {
                            from: None,
                            elapsed: 0.0,
                        });
                    }
                }
                // Neither kind's own: one command may carry these for both.
                let common = opacity.is_some() || interactive.is_some();
                if let Ok(mut gizmo) = transform_gizmos.get_mut(gizmo) {
                    if let Some(opacity) = at_once {
                        gizmo.opacity = opacity;
                    }
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
                    if let Some(opacity) = at_once {
                        gizmo.opacity = opacity;
                    }
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
                // Waits for a control added in this pass, as a configure does.
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
        app.world_mut().spawn((asset(), crate::robots::Incoming));
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
            .remove::<crate::robots::Incoming>()
            .insert((
                rig,
                EntityInstance {
                    id: ROBOT.into(),
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
                control: Control::Joint {
                    instance: ROBOT.into(),
                    joint: TILT.into(),
                    color: None,
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

    /// A transform control on the fixture's robot as a whole.
    fn on_robot() -> Control {
        Control::Transform {
            target: Target::Robot {
                instance: ROBOT.into(),
            },
            modes: None,
            local: false,
            colors: None,
        }
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
                control: Control::Joint {
                    instance: ROBOT.into(),
                    joint: TILT.into(),
                    color: None,
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
                    instance: ROBOT.into(),
                    joint: TILT.into(),
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
                control: Control::Joint {
                    instance: ROBOT.into(),
                    joint: TILT.into(),
                    color: None,
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
            opacity,
            interactive,
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

    /// Hidden by anything but the host, as a ghosted robot's controls are,
    /// a control takes an opacity at once as well.
    #[test]
    fn a_control_hidden_by_a_ghosting_takes_an_opacity_at_once() {
        let (mut app, body) = scene();
        robot(&mut app, body);
        let gizmo = add_goal(&mut app, "goal", None);
        app.world_mut().entity_mut(gizmo).insert(Visibility::Hidden);
        app.update();
        send(&mut app, configure("goal", Some(0.25), None));
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
