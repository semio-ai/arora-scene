//! Joints posed from their values.
//!
//! A joint's value is a feature like any other: the robot declares an
//! animatable for it, a host writes that animatable, and the body the joint
//! moves turns about the joint's axis or slides along it. The value is the
//! source of truth, as it is for every other feature, so a value written by a
//! device and one written by a dragged control land the same way.
//!
//! **A body is posed from its rest, never from where it last stood.** Its pose
//! is `rest * motion(value - rest value)`, where `rest` is the transform the
//! GLB gave it and the rest value is the joint's declared default, which is
//! the value it was exported at. Accumulating moves onto the current pose
//! would drift with every write.

use std::collections::HashMap;

use arora_visuals::joint_gizmo::JointMotion;
use bevy::prelude::*;

use crate::robot::{Joint, JointKind};

/// The joints the robot declares, read from its GLB before the app runs, as
/// the robot's metadata is.
#[derive(Resource, Default)]
pub struct RobotJoints(pub Vec<Joint>);

/// One body a joint's value moves.
#[derive(Clone, Debug)]
pub struct JointBinding {
    pub body: Entity,
    /// The body's transform as the GLB placed it.
    pub rest: Transform,
    /// The joint's value at `rest`.
    pub rest_value: f32,
    /// The axis the body turns about or slides along, in its own frame.
    pub axis: Dir3,
    pub motion: JointMotion,
    /// A mimic joint stands at `value * multiplier + offset` of the
    /// animatable it follows.
    pub mimic: Option<(f32, f32)>,
}

impl JointBinding {
    pub fn new(joint: &Joint, body: Entity, rest: Transform) -> Option<Self> {
        let motion = match joint.kind {
            JointKind::Revolute | JointKind::Continuous => JointMotion::Turn,
            JointKind::Prismatic => JointMotion::Slide,
            _ => return None,
        };
        Some(Self {
            body,
            rest,
            rest_value: joint.default,
            axis: Dir3::new(joint.axis).unwrap_or(Dir3::Y),
            motion,
            mimic: joint.mimic,
        })
    }

    /// The value this joint stands at when its animatable holds `value`.
    pub fn joint_value(&self, value: f32) -> f32 {
        match self.mimic {
            Some((multiplier, offset)) => value * multiplier + offset,
            None => value,
        }
    }

    /// The body's transform at joint value `value`.
    pub fn pose(&self, value: f32) -> Transform {
        let moved = value - self.rest_value;
        match self.motion {
            JointMotion::Turn => Transform {
                rotation: self.rest.rotation * Quat::from_axis_angle(*self.axis, moved),
                ..self.rest
            },
            JointMotion::Slide => Transform {
                translation: self.rest.translation + self.rest.rotation * *self.axis * moved,
                ..self.rest
            },
        }
    }
}

/// An instance's joints, by the animatable that drives them. Several joints
/// can follow one animatable: a mimic joint and the joint it mimics.
#[derive(Component, Default, Debug)]
pub struct JointRig(pub HashMap<String, Vec<JointBinding>>);

#[cfg(test)]
mod tests {
    use std::f32::consts::FRAC_PI_2;

    use super::*;

    fn joint(kind: JointKind, default: f32, mimic: Option<(f32, f32)>) -> Joint {
        Joint {
            name: "joint".into(),
            kind,
            axis: Vec3::Z,
            child_node: 0,
            child: "body".into(),
            animatable: Some("value".into()),
            mimic,
            min: -1.0,
            max: 1.0,
            default,
            color: Srgba::WHITE,
        }
    }

    fn rest() -> Transform {
        Transform::from_xyz(1.0, 2.0, 3.0).with_rotation(Quat::from_rotation_x(FRAC_PI_2))
    }

    fn bind(kind: JointKind, default: f32, mimic: Option<(f32, f32)>) -> Option<JointBinding> {
        JointBinding::new(&joint(kind, default, mimic), Entity::PLACEHOLDER, rest())
    }

    #[test]
    fn a_body_stands_at_rest_at_the_value_it_was_exported_at() {
        let binding = bind(JointKind::Revolute, 0.3, None).unwrap();
        assert_eq!(binding.pose(0.3), rest());
    }

    #[test]
    fn a_turn_is_about_the_axis_in_the_body_s_own_frame() {
        let posed = bind(JointKind::Revolute, 0.0, None)
            .unwrap()
            .pose(FRAC_PI_2);
        assert_eq!(posed.translation, rest().translation);
        let expected = rest().rotation * Quat::from_rotation_z(FRAC_PI_2);
        assert!(posed.rotation.abs_diff_eq(expected, 1e-6));
    }

    #[test]
    fn a_slide_is_along_the_axis_in_the_body_s_own_frame() {
        let posed = bind(JointKind::Prismatic, 0.0, None).unwrap().pose(0.5);
        // The rest's quarter turn about X carries the body's Z onto world -Y.
        assert!(posed
            .translation
            .abs_diff_eq(Vec3::new(1.0, 1.5, 3.0), 1e-6));
        assert_eq!(posed.rotation, rest().rotation);
    }

    #[test]
    fn a_mimic_joint_follows_its_leader_scaled_and_offset() {
        let binding = bind(JointKind::Revolute, 0.0, Some((-2.0, 0.1))).unwrap();
        assert!((binding.joint_value(0.5) - -0.9).abs() < 1e-6);
    }

    #[test]
    fn a_fixed_joint_binds_nothing() {
        assert!(bind(JointKind::Fixed, 0.0, None).is_none());
    }
}
