//! Drawing a frame only when something in it changed.
//!
//! A robot scene in an editor sits still most of the time, and a frame drawn
//! when nothing moved is the same picture again at the full cost of one. So
//! the app waits for winit to wake it — any input, a window event, a value
//! pushed from outside — and every frame decides whether it needs the next:
//!
//! - **anything moved**: a global transform is not where it was last frame.
//!   Compared by value, because propagation rewrites a child's transform
//!   whenever its parent's was written, moved or not. That covers
//!   the camera however it moves (a gesture, the controller's smoothing, the
//!   opening glide, an orbit), a gizmo drag, a joint, a bound value, a scene
//!   arriving. One rule rather than one per feature, so a new thing that
//!   moves needs nothing added here;
//! - **anything appeared or was restyled**: a mesh added, a mesh or material
//!   or image asset added or changed, a visibility toggled (by value, as
//!   transforms are), a selection or stage choice changed;
//! - **work is still owed**: the robots have not been measured for the first
//!   framing or one asked for, a robot has not spawned or not yet
//!   been reported drawn, or a pipeline is still compiling — each of which
//!   only finishes on a later frame;
//! - **something animates by itself**: the aurora's light changes every frame
//!   in its shader, so while a curtain is in view the scene ticks at
//!   [`Redraw::aurora_hz`] rather than at the display's rate.
//!
//! Requesting the next frame from the one that saw a change costs one frame
//! after the last change, which is what lets motion that continues on its own
//! — inertia, a glide — carry itself frame to frame.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bevy::prelude::*;
use bevy::render::render_resource::PipelineCache;
use bevy::render::{Render, RenderApp, RenderSystems};
use bevy::window::RequestRedraw;
use bevy::winit::{UpdateMode, WinitSettings};

use crate::aurora::AuroraCurtain;
use crate::stage::{BelowFloor, Palette, Stage};
use crate::Glide;

/// How the scene redraws when it is not changing.
#[derive(Resource, Clone, Copy, Debug)]
pub struct Redraw {
    /// Frames a second while an aurora curtain is in view. Its light drifts
    /// slowly; this is how smoothly.
    pub aurora_hz: f32,
}

impl Default for Redraw {
    fn default() -> Self {
        Self { aurora_hz: 30.0 }
    }
}

/// Whether the render world still has pipelines compiling, as of its last
/// frame. Shared by the two worlds, since a waiting pipeline is a render
/// world fact and the decision to draw again is the main world's.
#[derive(Resource, Clone, Default)]
struct PipelinesPending(Arc<AtomicBool>);

/// Puts the app in reactive mode and requests a frame whenever one is owed.
/// A host that wants continuous frames leaves it out.
pub struct RedrawOnChangePlugin;

impl Plugin for RedrawOnChangePlugin {
    fn build(&self, app: &mut App) {
        let pending = PipelinesPending::default();
        app.insert_resource(WinitSettings {
            focused_mode: UpdateMode::reactive(Duration::MAX),
            unfocused_mode: UpdateMode::reactive(Duration::MAX),
        })
        .init_resource::<Redraw>()
        .insert_resource(pending.clone())
        .add_systems(Last, (request_redraw, tick_for_aurora));
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app
                .insert_resource(pending)
                .add_systems(Render, note_pipelines.in_set(RenderSystems::Cleanup));
        }
    }
}

fn note_pipelines(cache: Res<PipelineCache>, pending: Res<PipelinesPending>) {
    pending.0.store(
        cache.waiting_pipelines().next().is_some(),
        Ordering::Relaxed,
    );
}

/// What says another frame is owed.
#[derive(bevy::ecs::system::SystemParam)]
struct Owed<'w, 's> {
    census: Res<'w, crate::Census>,
    /// Robots not yet reported drawn: the report reaches the main world only
    /// on a frame after the GPU finishes, so one is owed until it has.
    drawing: Query<'w, 's, (), With<crate::ready::Drawing>>,
    /// Every robot, loading or showing: an empty scene has nothing to frame,
    /// so its unframed camera owes nothing.
    robots: Query<'w, 's, (), With<crate::robots::RobotAsset>>,
    /// Active areas, with where each stands as it eases: one still on its way,
    /// or newly set, moves the view.
    areas: Query<
        'w,
        's,
        (
            Ref<'static, arora_visuals::active_area::ActiveArea>,
            Option<&'static arora_visuals::active_area::ActiveAreaShown>,
        ),
    >,
    pending: Res<'w, PipelinesPending>,
    glides: Query<'w, 's, (), With<Glide>>,
    moved: Query<'w, 's, (Entity, &'static GlobalTransform), Changed<GlobalTransform>>,
    /// Where each entity whose transform was written last stood.
    stood: Local<'s, bevy::ecs::entity::EntityHashMap<GlobalTransform>>,
    shown: Query<'w, 's, (Entity, &'static Visibility), Changed<Visibility>>,
    /// What each entity whose visibility was written was last set to.
    was: Local<'s, bevy::ecs::entity::EntityHashMap<Visibility>>,
    added: Query<'w, 's, (), Added<Mesh3d>>,
    unspawned: Query<'w, 's, (), (With<WorldAssetRoot>, Without<Children>)>,
    stage: Res<'w, Stage>,
    palette: Res<'w, Palette>,
    below: Res<'w, BelowFloor>,
    selections: Res<'w, arora_visuals::select::Selections>,
    meshes: MessageReader<'w, 's, AssetEvent<Mesh>>,
    materials: MessageReader<'w, 's, AssetEvent<StandardMaterial>>,
    images: MessageReader<'w, 's, AssetEvent<Image>>,
}

impl Owed<'_, '_> {
    /// Whether any written transform actually changed. Every one is looked at,
    /// so the record stays current for the next frame's comparison.
    fn moved_any(&mut self) -> bool {
        let mut moved = false;
        for (entity, global) in &self.moved {
            if self.stood.insert(entity, *global) != Some(*global) {
                moved = true;
            }
        }
        moved
    }

    /// Whether any written visibility actually changed, likewise by value:
    /// a system that sets the same visibility every frame shows nothing new.
    fn shown_any(&mut self) -> bool {
        let mut shown = false;
        for (entity, visibility) in &self.shown {
            if self.was.insert(entity, *visibility) != Some(*visibility) {
                shown = true;
            }
        }
        shown
    }

    fn any(&mut self) -> bool {
        let moved = self.moved_any();
        let shown = self.shown_any();
        // Every reader is drained, so a change is seen once.
        let assets =
            self.meshes.read().count() + self.materials.read().count() + self.images.read().count()
                > 0;
        let easing = self
            .areas
            .iter()
            .any(|(area, shown)| area.is_changed() || shown.is_none_or(|shown| shown.0 != area.0));
        assets
            || easing
            // Owed until the census is taken: a census that found nothing
            // to fit leaves the camera unfitted, and waiting on it would
            // never end.
            || (!self.census.taken && !self.robots.is_empty())
            || !self.drawing.is_empty()
            || self.pending.0.load(Ordering::Relaxed)
            || !self.glides.is_empty()
            || moved
            || shown
            || !self.added.is_empty()
            || !self.unspawned.is_empty()
            || self.stage.is_changed()
            || self.palette.is_changed()
            || self.below.is_changed()
            || self.selections.is_changed()
    }
}

fn request_redraw(mut owed: Owed, mut redraw: MessageWriter<RequestRedraw>) {
    if owed.any() {
        redraw.write(RequestRedraw);
    }
}

/// Ticks at the aurora's rate while a curtain was in view last frame, and not
/// at all otherwise.
fn tick_for_aurora(
    rate: Res<Redraw>,
    curtains: Query<&ViewVisibility, With<AuroraCurtain>>,
    mut settings: ResMut<WinitSettings>,
) {
    let wait = if rate.aurora_hz > 0.0 && curtains.iter().any(|seen| seen.get()) {
        Duration::from_secs_f32(1.0 / rate.aurora_hz)
    } else {
        Duration::MAX
    };
    let mode = UpdateMode::reactive(wait);
    if settings.focused_mode != mode {
        settings.focused_mode = mode;
        settings.unfocused_mode = mode;
    }
}
