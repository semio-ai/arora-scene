//! A command line over the scene.
//!
//! One host among several. The scene itself is the library beside this; what
//! lives here is flag parsing, the window, and the parts of a run that only a
//! desktop process can do — resolving assets from a directory and writing a
//! PNG to disk. A wasm host replaces exactly this file.

use std::path::{Path, PathBuf};

use arora_scene::{RobotSpec, SceneOptions, ScenePlugin};
use bevy::prelude::*;
use bevy::window::PresentMode;

/// Splits a path to an asset into the root Bevy resolves under and the name
/// within it.
///
/// Bevy has one asset root per app and every load is a name relative to it, so
/// a path typed on a command line has to be taken apart to be usable at all.
///
/// It is made absolute first, which is not tidiness. **Bevy resolves a
/// relative asset root against the crate directory under `cargo run` and
/// against the executable's directory otherwise — never against the working
/// directory the path was typed in.** A relative path would therefore mean
/// something different in each of those, and in two of the three it would mean
/// nothing. Resolving it here also turns a missing file into an error naming
/// the path, rather than an asset load that quietly never completes.
fn asset_root_and_name(path: &Path) -> anyhow::Result<(PathBuf, String)> {
    let full = path.canonicalize().map_err(|error| {
        anyhow::anyhow!("cannot read {}: {error}", path.display())
    })?;
    let name = full
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("{} names no file", path.display()))?
        .to_string_lossy()
        .into_owned();
    let root = full
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no directory", path.display()))?
        .to_path_buf();
    Ok((root, name))
}

#[derive(clap::Parser)]
struct Cli {
    /// The robot GLB to render.
    robot: PathBuf,

    /// Stop after this many seconds and report. Zero runs until closed.
    #[arg(long, default_value_t = 0.0)]
    seconds: f32,

    /// Hold the camera still instead of orbiting, to separate the cost of a
    /// moving view from the cost of the scene.
    #[arg(long, default_value_t = false)]
    still: bool,

    /// Where the camera starts, as a yaw in radians about the model. Defaults
    /// to looking the robot in the face.
    #[arg(long, default_value_t = 0.0)]
    angle: f32,

    /// How far above the model the camera starts, in radians.
    #[arg(long, default_value_t = 0.4)]
    pitch: f32,

    /// Drop shadows, to price them separately.
    #[arg(long, default_value_t = false)]
    no_shadows: bool,

    /// Present without waiting for vsync, so the frame time measures the work
    /// rather than the display's cadence.
    ///
    /// Note that wgpu falls back to Fifo where a platform cannot honour this —
    /// macOS among them, where it does nothing at all. Use `--offscreen`
    /// there.
    #[arg(long, default_value_t = false)]
    uncapped: bool,

    /// Spawn the robot this many times, in a grid.
    #[arg(long, default_value_t = 1)]
    copies: u32,

    /// Save one frame here, after the warm-up.
    #[arg(long)]
    screenshot: Option<PathBuf>,

    /// Add a reference cube at the model's centre and clear to magenta.
    #[arg(long, default_value_t = false)]
    marker: bool,

    /// Build the screen the robot declares, as a quad with its own texture.
    /// Nothing fills it yet — see `mount_screen`.
    #[arg(long, default_value_t = false)]
    screen: bool,

    /// Put a limited rotation gizmo on this joint, by name. Pass `list` to see
    /// which joints the robot declares and what limits they carry.
    #[arg(long)]
    joint: Option<String>,

    /// Hold the ring in its hovered state.
    #[arg(long, default_value_t = false)]
    ring_active: bool,

    /// Drive the joint to this value instead of its authored default.
    #[arg(long)]
    joint_value: Option<f32>,

    /// Give every mesh its own material, as a renderer must when each
    /// element's colour is separately drivable.
    #[arg(long, default_value_t = false)]
    unique_materials: bool,

    /// Render into an offscreen target and leave the window empty.
    #[arg(long, default_value_t = false)]
    offscreen: bool,
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = <Cli as clap::Parser>::parse();

    let (assets, glb) = match asset_root_and_name(&cli.robot) {
        Ok(split) => split,
        Err(error) => {
            log::error!("{error:#}");
            std::process::exit(2);
        }
    };

    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "arora-scene".into(),
                    resolution: (1280u32, 800u32).into(),
                    present_mode: if cli.uncapped {
                        PresentMode::AutoNoVsync
                    } else {
                        PresentMode::AutoVsync
                    },
                    ..default()
                }),
                ..default()
            })
            .set(AssetPlugin {
                file_path: assets.to_string_lossy().into_owned(),
                ..default()
            })
            .build()
            // The process installs its own logger before Bevy starts.
            .disable::<bevy::log::LogPlugin>(),
    )
    .add_plugins(ScenePlugin)
    .insert_resource(SceneOptions {
        still: cli.still,
        angle: cli.angle,
        pitch: cli.pitch,
        shadows: !cli.no_shadows,
        marker: cli.marker,
        unique_materials: cli.unique_materials,
        ring_active: cli.ring_active,
        offscreen: cli.offscreen,
        seconds: cli.seconds,
        screenshot: cli.screenshot.clone(),
        uncapped: cli.uncapped,
    })
    .insert_resource(RobotSpec {
        glb: glb.clone(),
        copies: cli.copies,
    });

    // Both read the GLB from disk before the app runs, which is why they are
    // the CLI's to call rather than the scene's to do: a wasm host has no
    // filesystem, and an editor loads its assets after startup. Making these
    // asset-driven loads is what would let either happen, and it is the same
    // change that lets a running scene gain and lose assets.
    if cli.screen {
        arora_scene::mount_screen(&mut app, &assets, &glb);
    }
    if let Some(joint) = &cli.joint {
        arora_scene::mount_joint(&mut app, &assets, &glb, joint, cli.joint_value);
    }
    app.run();
}
