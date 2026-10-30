// An aurora curtain: light hanging from a ribbon high over the night sky.
//
// `uv.x` runs along the ribbon and `uv.y` up the curtain. A curtain is
// brightest near its lower edge and fades upward into the dark; it fades in
// and out along its length so it has no ends; and it is
// broken into vertical rays that drift slowly along it. Drawn additively, so
// curtains crossing each other brighten rather than hide one another.

import bevy_pbr::render::{forward_io::VertexOutput, mesh_view_bindings::{globals, view}};
import super::horizon::above_hills;

struct Aurora {
    // The colour at the curtain's lower edge, and at its top.
    low: vec4<f32>,
    high: vec4<f32>,
    brightness: f32,
    _padding: vec3<f32>,
}

@group(constants::MATERIAL_BIND_GROUP) @binding(0) var<uniform> aurora: Aurora;

fn hash(x: f32) -> f32 {
    return fract(sin(x) * 43758.5453123);
}

fn noise(x: f32) -> f32 {
    let i = floor(x);
    let f = fract(x);
    return mix(hash(i), hash(i + 1.0), smoothstep(0.0, 1.0, f));
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let along = in.uv.x;
    let up = in.uv.y;
    let time = globals.time;

    // Every edge fades out over a wide band, so no curtain has a line where it
    // stops: the lower edge is the brightest part but still not a hard one,
    // and the ends thin out over a third of the length each.
    let lower_edge = smoothstep(0.0, 0.18, up);
    let fade_upward = pow(1.0 - up, 2.2);
    let ends = smoothstep(0.0, 0.35, along) * smoothstep(1.0, 0.65, along);
    let rays = 0.45 + 0.55 * noise(along * 140.0 + time * 0.35) * noise(along * 31.0 - time * 0.15);

    // Green holds through the lower third, as it does in a real curtain, and
    // gives way to the upper colour above it.
    let colour = mix(aurora.low.rgb, aurora.high.rgb, smoothstep(0.25, 0.7, up));
    // The curtain hangs far nearer than the sky dome, so depth alone would put
    // it in front of the hills; it is hidden behind them by their silhouette.
    let dir = normalize(in.world_position.xyz - view.world_position);
    let strength = aurora.brightness * lower_edge * fade_upward * ends * rays * above_hills(dir);
    return vec4(colour * strength, strength);
}
