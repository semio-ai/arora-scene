//! A host drawing an instance's robot as a ghost.
//!
//! The host asks by instance ([`RobotRequest::Ghost`](crate::robots::RobotRequest)),
//! with a style or none to make it solid again. The choice is kept by the
//! instance, so a robot swapped into it is ghosted the same way, until the
//! host changes it or unloads the instance. A style changed while it shows —
//! a ghost fading out, say — changes the ghost from the next frame, through
//! the shared layer's [`GhostStyle`].
//!
//! Ghosting done some other way — the scene's own option that ghosts every
//! robot, or the debug panel — is left alone: only a ghost the host asked for
//! is taken away when it asks for the robot solid.

use std::collections::HashMap;

use arora_visuals::ghost::{Ghost, GhostDepth, GhostStyle};
use bevy::color::Srgba;
use bevy::prelude::*;
use serde::Deserialize;

use crate::robots::RobotAsset;

/// The instances a host has asked to see as ghosts, and how.
#[derive(Resource, Default)]
pub struct GhostedInstances(pub HashMap<String, GhostStyle>);

/// On a robot the host ghosted, so asking for it solid takes away only that.
#[derive(Component)]
pub struct GhostedByHost;

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

/// Draws each robot as its instance's host asked: a ghost in its style, or
/// solid again. Runs over the robots present, which are few, so a robot
/// loaded into a ghosted instance is ghosted the frame it appears.
pub fn ghost_robots(
    mut commands: Commands,
    ghosted: Res<GhostedInstances>,
    roots: Query<(Entity, &RobotAsset, Option<&GhostStyle>, Has<GhostedByHost>)>,
) {
    for (root, asset, style, by_host) in &roots {
        match ghosted.0.get(&asset.instance) {
            Some(wanted) => {
                if style != Some(wanted) || !by_host {
                    commands
                        .entity(root)
                        .insert((Ghost, *wanted, GhostedByHost));
                }
            }
            None if by_host => {
                commands
                    .entity(root)
                    .remove::<(Ghost, GhostStyle, GhostedByHost)>();
            }
            None => {}
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
