//! A host drawing an instance's robot as a ghost.
//!
//! The host asks by instance ([`RobotRequest::Ghost`](crate::robots::RobotRequest)),
//! with a style or none to make it solid again. The choice is kept by the
//! instance, so a robot swapped into it is ghosted the same way, until the
//! host changes it or unloads the instance. A style changed while it shows —
//! a ghost fading out, say — changes the ghost from the next frame, through
//! the shared layer's [`GhostStyle`].
//!
//! **Asking for a robot solid takes away only what the host put on.** The
//! host's style always goes; the ghost itself goes only if the host's call is
//! what made the robot a ghost, and never while the scene's own option ghosts
//! every robot. So a robot the option or the debug panel ghosted stays a
//! ghost, in the scene's look, once the host is done styling it.
//!
//! **The debug panel's choice holds until the host next asks.** The panel
//! turning a host's ghost solid leaves the host's style on it, unused, so the
//! robot stays solid while the host's style is unchanged; a different style
//! from the host ghosts it again. The panel turning ghosts on changes nothing
//! on a robot the host already ghosted.

use std::collections::HashMap;

use arora_visuals::ghost::{Ghost, GhostDepth, GhostStyle};
use bevy::color::Srgba;
use bevy::prelude::*;
use serde::Deserialize;

use crate::robots::RobotAsset;

/// The instances a host has asked to see as ghosts, and how.
#[derive(Resource, Default)]
pub struct GhostedInstances(pub HashMap<String, GhostStyle>);

/// On a robot the host ghosted: whether the host's call is what made it a
/// ghost, so asking for it solid takes away only what the host put on.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct GhostedByHost {
    pub made_it_a_ghost: bool,
}

/// A ghost's style as a host writes it: CSS hex colours, and what it leaves
/// out taken from the scene's look.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct GhostOptions {
    /// The colour every element is drawn in, in place of its own.
    pub color: Option<String>,
    /// The colour the ghost is washed toward.
    pub wash: Option<String>,
    /// How opaque the rim is against the scene's look.
    pub rim: Option<f32>,
    /// How opaque the whole ghost is, from nought to one.
    pub opacity: Option<f32>,
    /// `"overEverything"`, as a ghost beside its robot is drawn, or
    /// `"tested"`, hidden by what stands in front of it.
    pub depth: Option<Depth>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Depth {
    OverEverything,
    Tested,
}

impl GhostOptions {
    /// The style these options ask for, or why they are not one.
    pub fn style(&self) -> Result<GhostStyle, String> {
        let colour = |hex: &Option<String>| -> Result<Option<Color>, String> {
            hex.as_deref()
                .map(|hex| {
                    Srgba::hex(hex)
                        .map(Color::from)
                        .map_err(|_| format!("{hex:?} is not a hex colour"))
                })
                .transpose()
        };
        Ok(GhostStyle {
            colour: colour(&self.color)?,
            wash: colour(&self.wash)?,
            rim: self.rim,
            opacity: self.opacity.unwrap_or(1.0).clamp(0.0, 1.0),
            depth: match self.depth {
                None | Some(Depth::OverEverything) => GhostDepth::OverEverything,
                Some(Depth::Tested) => GhostDepth::Tested,
            },
        })
    }
}

/// What [`ghost_robots`] reads of each robot: its instance, the style on
/// it, whether it is a ghost, and whether the host ghosted it.
type HostGhosting = (
    Entity,
    &'static RobotAsset,
    Option<&'static GhostStyle>,
    Has<Ghost>,
    Option<&'static GhostedByHost>,
);

/// Draws each robot as its instance's host asked: a ghost in its style, or
/// solid again ([module docs](self)). Runs over the robots present, which are
/// few, so a robot loaded into a ghosted instance is ghosted the frame it
/// appears.
pub fn ghost_robots(
    mut commands: Commands,
    ghosted: Res<GhostedInstances>,
    opts: Option<Res<crate::SceneOptions>>,
    roots: Query<HostGhosting>,
) {
    let every_robot_a_ghost = opts.is_some_and(|opts| opts.ghost);
    for (root, asset, style, is_ghost, by_host) in &roots {
        match (ghosted.0.get(&asset.instance), by_host) {
            (Some(wanted), None) => {
                commands.entity(root).insert((
                    Ghost,
                    *wanted,
                    GhostedByHost {
                        made_it_a_ghost: !is_ghost,
                    },
                ));
            }
            (Some(wanted), Some(_)) if style != Some(wanted) => {
                let mut entity = commands.entity(root);
                entity.insert(*wanted);
                // Made solid since by the panel: the host's new style is
                // what makes it a ghost again.
                if !is_ghost {
                    entity.insert((
                        Ghost,
                        GhostedByHost {
                            made_it_a_ghost: true,
                        },
                    ));
                }
            }
            (Some(_), Some(_)) => {}
            (None, Some(by_host)) => {
                let mut entity = commands.entity(root);
                entity.remove::<(GhostStyle, GhostedByHost)>();
                if by_host.made_it_a_ghost && !every_robot_a_ghost {
                    entity.remove::<Ghost>();
                }
            }
            (None, None) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(instance: &str) -> RobotAsset {
        RobotAsset {
            instance: instance.into(),
            key: None,
            glb: String::new(),
            meta: Default::default(),
            joints: Vec::new(),
        }
    }

    /// A ghost the host asks for is drawn in its style, follows a change to
    /// it, and goes when the host asks for the robot solid; a ghost put on
    /// some other way stays.
    #[test]
    fn a_host_ghosts_and_unghosts_its_robots() {
        let mut app = App::new();
        app.init_resource::<GhostedInstances>()
            .add_systems(Update, ghost_robots);
        let a = app.world_mut().spawn(asset("a")).id();
        let other = app.world_mut().spawn((asset("b"), Ghost)).id();
        let faded = GhostStyle {
            opacity: 0.5,
            ..default()
        };
        app.world_mut()
            .resource_mut::<GhostedInstances>()
            .0
            .insert("a".into(), faded);
        app.update();
        assert!(app.world().get::<Ghost>(a).is_some());
        assert_eq!(app.world().get::<GhostStyle>(a), Some(&faded));

        let fainter = GhostStyle {
            opacity: 0.1,
            ..default()
        };
        app.world_mut()
            .resource_mut::<GhostedInstances>()
            .0
            .insert("a".into(), fainter);
        app.update();
        assert_eq!(app.world().get::<GhostStyle>(a), Some(&fainter));

        app.world_mut().resource_mut::<GhostedInstances>().0.clear();
        app.update();
        assert!(app.world().get::<Ghost>(a).is_none(), "solid again");
        assert!(app.world().get::<GhostStyle>(a).is_none());
        assert!(
            app.world().get::<Ghost>(other).is_some(),
            "a ghost the host did not ask for is left"
        );
    }

    /// Asking for a robot solid takes away only what the host put on: a
    /// robot already a ghost, or ghosted by the scene's option, stays one.
    #[test]
    fn clearing_a_hosts_ghost_leaves_a_ghost_it_did_not_make() {
        let mut app = App::new();
        app.init_resource::<GhostedInstances>()
            .add_systems(Update, ghost_robots);
        let already = app.world_mut().spawn((asset("a"), Ghost)).id();
        app.world_mut()
            .resource_mut::<GhostedInstances>()
            .0
            .insert("a".into(), GhostStyle::default());
        app.update();
        app.world_mut().resource_mut::<GhostedInstances>().0.clear();
        app.update();
        assert!(
            app.world().get::<Ghost>(already).is_some(),
            "it was a ghost before"
        );
        assert!(
            app.world().get::<GhostStyle>(already).is_none(),
            "the host's style went"
        );
        assert!(app.world().get::<GhostedByHost>(already).is_none());

        // With the scene's option ghosting every robot, the host's clearing
        // leaves the ghost the option keeps, though the host put it on first.
        app.insert_resource(crate::SceneOptions {
            ghost: true,
            ..default()
        });
        let later = app.world_mut().spawn(asset("b")).id();
        app.world_mut()
            .resource_mut::<GhostedInstances>()
            .0
            .insert("b".into(), GhostStyle::default());
        app.update();
        app.world_mut().resource_mut::<GhostedInstances>().0.clear();
        app.update();
        assert!(
            app.world().get::<Ghost>(later).is_some(),
            "the option's ghost"
        );
    }

    /// The panel making a host's ghost solid holds while the host's style is
    /// unchanged; a different style ghosts it again, and the host's clearing
    /// then makes it solid.
    #[test]
    fn the_panel_holds_until_the_host_next_asks() {
        let mut app = App::new();
        app.init_resource::<GhostedInstances>()
            .add_systems(Update, ghost_robots);
        let robot = app.world_mut().spawn(asset("a")).id();
        let ask = |app: &mut App, opacity: f32| {
            app.world_mut().resource_mut::<GhostedInstances>().0.insert(
                "a".into(),
                GhostStyle {
                    opacity,
                    ..default()
                },
            );
            app.update();
        };
        ask(&mut app, 0.5);
        app.world_mut().entity_mut(robot).remove::<Ghost>();
        app.update();
        assert!(
            app.world().get::<Ghost>(robot).is_none(),
            "the panel's solid holds"
        );
        ask(&mut app, 0.4);
        assert!(
            app.world().get::<Ghost>(robot).is_some(),
            "a new style ghosts it"
        );
        app.world_mut().resource_mut::<GhostedInstances>().0.clear();
        app.update();
        assert!(
            app.world().get::<Ghost>(robot).is_none(),
            "the host's ghost goes"
        );
    }

    /// Options read a wash, and an opacity outside nought to one is held to
    /// it.
    #[test]
    fn options_read_a_wash_and_hold_the_opacity() {
        let options: GhostOptions =
            serde_json::from_str(r##"{ "wash": "#000000", "opacity": 3 }"##).unwrap();
        let style = options.style().unwrap();
        assert_eq!(style.wash, Some(Color::from(Srgba::BLACK)));
        assert_eq!(style.opacity, 1.0);
        let below: GhostOptions = serde_json::from_str(r#"{ "opacity": -1 }"#).unwrap();
        assert_eq!(below.style().unwrap().opacity, 0.0);
    }

    /// Options read as the style they describe; a bad colour is refused.
    #[test]
    fn options_read_as_a_style() {
        let options: GhostOptions =
            serde_json::from_str(r##"{ "color": "#ff0000", "opacity": 0.25, "depth": "tested" }"##)
                .unwrap();
        let style = options.style().unwrap();
        assert_eq!(style.colour, Some(Color::from(Srgba::RED)));
        assert_eq!(style.opacity, 0.25);
        assert_eq!(style.depth, GhostDepth::Tested);
        assert_eq!(style.wash, None);

        let plain = GhostOptions::default().style().unwrap();
        assert_eq!(plain, GhostStyle::default());

        let bad: GhostOptions = serde_json::from_str(r#"{ "color": "nope" }"#).unwrap();
        assert!(bad.style().is_err());
    }
}
