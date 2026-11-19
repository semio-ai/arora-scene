//! When a robot is drawn: once the GPU has finished a frame drawing every
//! mesh of it the scene's camera sees, each with its pipeline compiled.
//!
//! **Loaded is not drawn.** A robot that has loaded whole, spawned and been
//! indexed can still draw nothing for a while: creating a pipeline returns
//! before some backends have compiled it — a browser's WebGL compiles in its
//! GPU process, when a draw first uses the program. A host showing a spinner
//! until a robot appears, or timing how long one takes, needs the frame it
//! actually drew, which is what [`RobotsReady`] reports, one instance at a
//! time.
//!
//! The scene draws every robot through one camera, so this watches that
//! camera's view for each robot's own meshes, rather than ordering a camera
//! per instance as the shared layer's `drawn` does. A robot with nothing in
//! the camera's view has nothing to wait for, and is ready at once.
//!
//! **Nothing holds a robot back for good.** A pipeline that failed for good
//! never draws and the renderer logs it, so its mesh counts as settled.
//! Anything else — a camera that draws no frame, a GPU that never reports the
//! frame done — is given up on after [`GIVE_UP_FRAMES`] frames, and the
//! robot is reported ready as it draws. The scene keeps drawing while a robot
//! is unreported (see `redraw`), so those frames come.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;

use bevy::pbr::{PendingMeshMaterialQueues, SpecializedMaterialPipelineCache};
use bevy::prelude::*;
use bevy::render::render_resource::{CachedPipelineState, PipelineCache};
use bevy::render::renderer::RenderQueue;
use bevy::render::sync_world::MainEntity;
use bevy::render::view::{ExtractedView, RenderVisibleEntities, RetainedViewEntity};
use bevy::render::{Extract, ExtractSchedule, Render, RenderApp, RenderSystems};
use bevy::shader::ShaderCacheError;

use crate::robots::RobotAsset;
use crate::OrbitCamera;

/// Frames after which a robot still not drawn is reported ready as it draws:
/// ten seconds at 60 Hz, long enough for a slow pipeline compile on a CPU
/// renderer, short enough that one which can never draw whole does not hold a
/// host for good.
const GIVE_UP_FRAMES: u32 = 600;

/// On a robot's root from the frame it shows until it is drawn.
#[derive(Component)]
pub struct Drawing;

/// The instances drawn since a host last took them, in the order they were.
#[derive(Resource, Default)]
pub struct RobotsReady(pub Vec<String>);

/// The render world's reports of drawn roots, read by the main world.
#[derive(Resource)]
struct Reports(Mutex<Receiver<Entity>>);

/// The render world's end of [`Reports`].
#[derive(Resource)]
struct ReportSender(Sender<Entity>);

/// One drawing robot, as the render world follows it.
#[derive(Default)]
struct Watch {
    /// Its meshes.
    meshes: HashSet<MainEntity>,
    /// Frames since it started drawing.
    frames: u32,
    /// Whether the frame drawing it whole was submitted.
    submitted: bool,
    /// Whether it was reported without waiting any longer.
    given_up: bool,
}

/// The drawing robots, by root, and the scene camera's view.
#[derive(Resource, Default)]
struct Watched {
    robots: HashMap<Entity, Watch>,
    view: Option<RetainedViewEntity>,
}

pub struct ReadyPlugin;

impl Plugin for ReadyPlugin {
    fn build(&self, app: &mut App) {
        let (sender, receiver) = channel();
        app.insert_resource(Reports(Mutex::new(receiver)))
            .init_resource::<RobotsReady>()
            .add_systems(Update, ready_drawn_robots);
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app
                .insert_resource(ReportSender(sender))
                .init_resource::<Watched>()
                .add_systems(ExtractSchedule, extract_drawing)
                // After the frame is submitted, so the GPU callback is about
                // this frame's work.
                .add_systems(Render, watch_drawing.in_set(RenderSystems::Cleanup));
        }
    }
}

/// Follow each robot still drawing, with its meshes, and the scene's camera.
fn extract_drawing(
    robots: Extract<Query<Entity, With<Drawing>>>,
    children: Extract<Query<&Children>>,
    meshes: Extract<Query<(), With<Mesh3d>>>,
    cameras: Extract<Query<Entity, With<OrbitCamera>>>,
    mut watched: ResMut<Watched>,
) {
    watched.view = cameras
        .iter()
        .next()
        .map(|camera| RetainedViewEntity::new(camera.into(), None, 0));
    let drawing: HashSet<Entity> = robots.iter().collect();
    watched.robots.retain(|root, _| drawing.contains(root));
    for root in drawing {
        let watch = watched.robots.entry(root).or_default();
        watch.frames += 1;
        if watch.meshes.is_empty() {
            watch.meshes = children
                .iter_descendants(root)
                .filter(|entity| meshes.contains(*entity))
                .map(MainEntity::from)
                .collect();
        }
    }
}

/// For each drawing robot whose every mesh in the camera's view was drawn
/// this frame, have the GPU report when it has finished the frame; give up on
/// one held back too long, whether by its meshes, a view that draws nothing,
/// or a GPU that has not reported.
fn watch_drawing(
    views: Query<(&ExtractedView, &RenderVisibleEntities)>,
    mut watched: ResMut<Watched>,
    specialized: Res<SpecializedMaterialPipelineCache>,
    pending: Res<PendingMeshMaterialQueues>,
    pipelines: Res<PipelineCache>,
    queue: Res<RenderQueue>,
    sender: Res<ReportSender>,
) {
    for (root, watch) in &mut watched.robots {
        if watch.frames >= GIVE_UP_FRAMES && !watch.given_up {
            watch.given_up = true;
            watch.submitted = true;
            let _ = sender.0.send(*root);
        }
    }
    let Some(id) = watched.view else {
        return;
    };
    let Some((_, visible)) = views
        .iter()
        .find(|(view, _)| view.retained_view_entity == id)
    else {
        return;
    };
    let view_pipelines = specialized.get(&id);
    let queued = pending.get(&id).map(|queues| &queues.current_frame);
    // Each visible mesh, and whether it was drawn this frame: queued, with a
    // pipeline as it will stay.
    let drawn: HashMap<MainEntity, bool> = visible
        .get::<Mesh3d>()
        .into_iter()
        .flat_map(|meshes| meshes.iter_visible())
        .map(|(entity, main)| {
            let in_queue = !queued.is_some_and(|q| q.contains(&(*entity, *main)));
            let compiled = view_pipelines
                .and_then(|ids| ids.get(main))
                .is_some_and(|pipeline| settled(pipelines.get_render_pipeline_state(*pipeline)));
            (*main, in_queue && compiled)
        })
        .collect();
    for (root, watch) in &mut watched.robots {
        if watch.submitted {
            continue;
        }
        let whole = watch
            .meshes
            .iter()
            .all(|mesh| drawn.get(mesh).copied().unwrap_or(true));
        if whole {
            watch.submitted = true;
            let sender = sender.0.clone();
            let root = *root;
            queue.on_submitted_work_done(move || {
                // The app may be gone by the time the GPU finishes.
                let _ = sender.send(root);
            });
        }
    }
}

/// Whether a pipeline is as it will stay: compiled, or failed for good. A
/// pipeline waiting on a shader to load fails and is queued again, so that
/// failure is not final.
fn settled(state: &CachedPipelineState) -> bool {
    match state {
        CachedPipelineState::Ok(_) => true,
        CachedPipelineState::Err(
            ShaderCacheError::ShaderNotLoaded(_) | ShaderCacheError::ShaderImportNotYetAvailable,
        ) => false,
        CachedPipelineState::Err(_) => true,
        CachedPipelineState::Queued | CachedPipelineState::Creating(_) => false,
    }
}

/// Reports each robot the render world says is drawn, once: a robot given up
/// on can be reported again when its frame does finish.
fn ready_drawn_robots(
    reports: Res<Reports>,
    robots: Query<&RobotAsset, With<Drawing>>,
    mut ready: ResMut<RobotsReady>,
    mut commands: Commands,
) {
    let Ok(reports) = reports.0.lock() else {
        return;
    };
    let mut told = HashSet::new();
    for root in reports.try_iter() {
        let Ok(robot) = robots.get(root) else {
            continue;
        };
        if !told.insert(root) {
            continue;
        }
        commands.entity(root).remove::<Drawing>();
        ready.0.push(robot.instance.clone());
        log::info!("{}: drawn", robot.instance);
    }
}
