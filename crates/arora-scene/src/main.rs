//! A command line over the scene.
//!
//! One host among several. The scene itself is the library beside this; what
//! lives here is flag parsing, the window, and the parts of a run that only a
//! desktop process can do — resolving assets from a directory and writing a
//! PNG to disk. A wasm host replaces exactly this file.

use std::path::{Path, PathBuf};

use arora_scene::stage::{Environment, Lighting, Theme};
use arora_scene::{RobotSpec, SceneOptions, ScenePlugin};
use arora_visuals::select::{Highlight, Selection};
use arora_visuals::transform_gizmo::{GizmoSpace, TransformModes};
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
    let full = path
        .canonicalize()
        .map_err(|error| anyhow::anyhow!("cannot read {}: {error}", path.display()))?;
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

    /// Outline these elements, by the ids `RobotData` gives them, each
    /// optionally in a colour of its own: `ID` or `ID=#rrggbb`. Repeatable.
    #[arg(long = "select", value_parser = parse_selection)]
    select: Vec<Selection>,

    /// Print the elements the robot declares and exit.
    #[arg(long, default_value_t = false)]
    list_elements: bool,

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

    /// Light or dark.
    #[arg(long, value_enum, default_value_t)]
    theme: Theme,

    /// What fills the frame above the horizon.
    #[arg(long, value_enum, default_value_t)]
    environment: Environment,

    /// What the rig is fixed to.
    #[arg(long, value_enum, default_value_t)]
    lighting: Lighting,

    /// How strongly a selection is filled with its outline colour, from 0 to 1.
    #[arg(long, default_value_t = 0.0)]
    outline_fill: f32,

    /// Show sliders for tuning the current theme's floor and sky by eye.
    #[arg(long, default_value_t = false)]
    tune: bool,

    /// Put a transform gizmo on the robot, offering these modes: `all`, or any
    /// of `move`, `turn` and `scale` joined by commas. Hold M, R or S to show
    /// only one.
    #[arg(long, value_parser = parse_modes)]
    gizmo: Option<TransformModes>,

    /// Move and turn the gizmo along the robot's own axes rather than the
    /// world's.
    #[arg(long, default_value_t = false)]
    gizmo_local: bool,

    /// Draw the robot as a ghost, as Studio draws a robot's measured state.
    #[arg(long, default_value_t = false)]
    ghost: bool,

    /// What a ghost is washed toward: `white`, as Studio has it, or `theme`,
    /// the theme's ink, so a ghost reads on the light ground too.
    #[arg(long, default_value = "white", value_parser = ["white", "theme"])]
    ghost_wash: String,

    /// How strong a ghost's rim is against Studio's on the light theme, when
    /// washing toward the theme.
    #[arg(long, default_value_t = 1.0)]
    ghost_rim: f32,

    /// The aurora's three colours from its lower edge up, as
    /// `#rrggbb,#rrggbb,#rrggbb`.
    #[arg(long, value_parser = parse_colours)]
    aurora_colours: Option<[Color; 3]>,
}

fn parse_modes(arg: &str) -> Result<TransformModes, String> {
    if arg == "all" {
        return Ok(TransformModes::ALL);
    }
    let mut modes = TransformModes {
        translate: false,
        rotate: false,
        scale: false,
    };
    for mode in arg.split(',') {
        match mode.trim() {
            "move" => modes.translate = true,
            "turn" => modes.rotate = true,
            "scale" => modes.scale = true,
            other => return Err(format!("{other} is not a mode: use move, turn or scale")),
        }
    }
    Ok(modes)
}

fn parse_colours(arg: &str) -> Result<[Color; 3], String> {
    let colours = arg
        .split(',')
        .map(|hex| {
            Srgba::hex(hex.trim())
                .map(Color::from)
                .map_err(|error| format!("{hex} is not a colour: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    colours
        .try_into()
        .map_err(|found: Vec<Color>| format!("expected three colours, found {}", found.len()))
}

/// `ID` or `ID=#rrggbb`.
fn parse_selection(arg: &str) -> Result<Selection, String> {
    let Some((element, hex)) = arg.split_once('=') else {
        return Ok(Selection::of(arg));
    };
    let colour = Srgba::hex(hex).map_err(|error| format!("{hex} is not a colour: {error}"))?;
    Ok(Selection {
        element: element.to_string(),
        highlight: Some(Highlight {
            colour: Some(colour.into()),
            ..default()
        }),
    })
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
        theme: cli.theme,
        environment: cli.environment,
        lighting: cli.lighting,
        outline_fill: cli.outline_fill,
        gizmo: cli.gizmo,
        ghost: cli.ghost,
        ghost_wash_theme: cli.ghost_wash == "theme",
        ghost_rim: cli.ghost_rim,
        gizmo_space: if cli.gizmo_local {
            GizmoSpace::Local
        } else {
            GizmoSpace::World
        },
    })
    .insert_resource(RobotSpec {
        glb: glb.clone(),
        copies: cli.copies,
    });
    if let Some(colours) = cli.aurora_colours {
        app.insert_resource(arora_scene::aurora::Aurora {
            colours,
            ..default()
        });
    }

    // Both read the GLB from disk before the app runs, which is why they are
    // the CLI's to call rather than the scene's to do: a wasm host has no
    // filesystem, and an editor loads its assets after startup. Making these
    // asset-driven loads is what would let either happen, and it is the same
    // change that lets a running scene gain and lose assets.
    if cli.list_elements {
        match std::fs::read(assets.join(&glb))
            .map_err(anyhow::Error::from)
            .and_then(|bytes| arora_visuals::meta::AssetMeta::from_glb_bytes(&bytes))
        {
            Ok(meta) => {
                log::info!("{} elements:", meta.elements.len());
                for element in &meta.elements {
                    log::info!(
                        "  {:38} {:10} node {}",
                        element.id,
                        element.kind,
                        element.node_name
                    );
                }
            }
            Err(error) => log::error!("{error:#}"),
        }
        return;
    }

    arora_scene::mount_instances(&mut app, &assets, &glb, cli.select.clone());
    if cli.screen {
        arora_scene::mount_screen(&mut app, &assets, &glb);
    }
    if cli.tune {
        app.add_plugins(arora_scene::tune::TunePlugin);
    }
    if let Some(joint) = &cli.joint {
        arora_scene::mount_joint(&mut app, &assets, &glb, joint, cli.joint_value);
    }
    app.run();
}
