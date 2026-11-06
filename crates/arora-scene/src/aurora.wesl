// An aurora curtain: light hanging from a ribbon high over the night sky.
//
// The vertex stage hangs and sways the ribbon: each vertex carries only its
// place along the ribbon and whether it is the lower or upper edge, in `uv`,
// and is moved to where the wave has it now, the same motion for every frame
// at no cost to the CPU.
//
// `uv.x` runs along the ribbon and `uv.y` up the curtain. A curtain is
// brightest near its lower edge and fades upward into the dark; it fades in
// and out along its length so it has no ends; and it is
// broken into vertical rays that drift slowly along it. Drawn additively, so
// curtains crossing each other brighten rather than hide one another.
//
// Colour is a separate channel from that brightness: a slowly changing field
// of noise over the world picks where on the three-colour gradient each point
// falls, so curtains differ from one another and from moment to moment.

import bevy_pbr::render::{
    forward_io::{Vertex, VertexOutput, decompress_vertex},
    mesh_view_bindings::{globals, view},
    view_transformations::position_world_to_clip,
};
import super::horizon::above_hills;

struct Aurora {
    // The gradient from the lower edge up; see `Aurora::colours`.
    colours: array<vec4<f32>, 3>,
    brightness: f32,
    // How far the curtain fades where it is seen edge-on; see `Aurora`.
    edge_fade: f32,
    // The colour field's patch size in metres, its rate of change, and how
    // far it leans up the gradient toward a curtain's top; see `Aurora`.
    colour_scale: f32,
    colour_speed: f32,
    colour_lift: f32,
    _padding: vec3<f32>,
    // offset, from, to, lift; height, bend, bends, phase; wave, speed,
    // altitude: see `Curtain` and `Aurora`.
    place: vec4<f32>,
    form: vec4<f32>,
    motion: vec4<f32>,
    // The way the curtains run, and the way they are spaced.
    forward: vec4<f32>,
    right: vec4<f32>,
}

const TAU: f32 = 6.283185307;
const PI: f32 = 3.141592654;

// Where a point of the curtain is now: `along` it from nought to one, on its
// lower edge (`up` nought) or its upper (`up` one). The ribbon sways side to
// side in two waves of different lengths travelling in opposite directions,
// its lower edge rises and falls a little, and the top lags the bottom so the
// curtain folds rather than moving as a flat sheet.
fn hang(along: f32, up: f32) -> vec3<f32> {
    let offset = aurora.place.x;
    let ahead = mix(aurora.place.y, aurora.place.z, along);
    let lift = aurora.place.w;
    let height = aurora.form.x;
    let bend = aurora.form.y;
    let bends = aurora.form.z;
    let phase = aurora.form.w;
    let wave = aurora.motion.x;
    let t = globals.time * aurora.motion.y * TAU;
    let altitude = aurora.motion.z;
    let lag = 0.5 * up;
    let sway = wave * sin(TAU * 2.3 * along + t + phase + lag)
        + 0.5 * wave * sin(TAU * 5.1 * along - 0.7 * t + phase);
    let rise = 0.2 * wave * sin(TAU * 3.7 * along - 1.3 * t + phase);
    let bent = offset + bend * sin(PI * bends * along + phase);
    return aurora.forward.xyz * ahead
        + vec3(0.0, altitude + lift + rise + height * up, 0.0)
        + aurora.right.xyz * (bent + sway);
}

@vertex
fn vertex(vertex_in: Vertex) -> VertexOutput {
    let vertex = decompress_vertex(vertex_in, vertex_in.instance_index);
    let along = vertex.uv.x;
    let up = vertex.uv.y;
    let position = hang(along, up);
    // Across the sheet: from the run of the lower edge either side of this
    // column and the column's own rise, so the edge fade turns smoothly along
    // the ribbon.
    let step = 1.0 / 240.0;
    let run = hang(min(along + step, 1.0), 0.0) - hang(max(along - step, 0.0), 0.0);
    let rise = hang(along, 1.0) - hang(along, 0.0);
    var normal = cross(run, rise);
    if (dot(normal, normal) < 1e-12) {
        normal = aurora.right.xyz;
    }
    var out: VertexOutput;
    out.world_position = vec4(position, 1.0);
    out.position = position_world_to_clip(position);
    out.world_normal = normalize(normal);
    out.uv = vertex.uv;
    return out;
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

fn hash3(p: vec3<f32>) -> f32 {
    let q = fract(p * 0.3183099 + vec3(0.71, 0.113, 0.419));
    let r = q * 17.0;
    return fract(r.x * r.y * r.z * (r.x + r.y + r.z));
}

// Value noise over space, from nought to one.
fn noise3(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let x00 = mix(hash3(i), hash3(i + vec3(1.0, 0.0, 0.0)), u.x);
    let x10 = mix(hash3(i + vec3(0.0, 1.0, 0.0)), hash3(i + vec3(1.0, 1.0, 0.0)), u.x);
    let x01 = mix(hash3(i + vec3(0.0, 0.0, 1.0)), hash3(i + vec3(1.0, 0.0, 1.0)), u.x);
    let x11 = mix(hash3(i + vec3(0.0, 1.0, 1.0)), hash3(i + vec3(1.0, 1.0, 1.0)), u.x);
    return mix(mix(x00, x10, u.y), mix(x01, x11, u.y), u.z);
}

// Noise over space that changes in time: two unrelated fields, blended from
// one into the next over each unit of `t`. It changes in place rather than
// sliding, as an aurora's colour shifts without its curtains moving.
fn evolving(p: vec3<f32>, t: f32) -> f32 {
    let k = floor(t);
    let shift = vec3(17.31, 5.73, 11.13);
    let a = noise3(p + shift * k);
    let b = noise3(p + shift * (k + 1.0));
    return mix(a, b, smoothstep(0.0, 1.0, fract(t)));
}

// The three colours as a gradient over `g` from nought to one, each taking a
// broad span of it so that the field shows all three rather than the middle.
fn gradient(g: f32) -> vec3<f32> {
    let low = mix(aurora.colours[0].rgb, aurora.colours[1].rgb, smoothstep(0.3, 0.5, g));
    return mix(low, aurora.colours[2].rgb, smoothstep(0.55, 0.75, g));
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

    let p = in.world_position.xyz / max(aurora.colour_scale, 1.0);
    let t = time * aurora.colour_speed;
    let field = 0.65 * evolving(p, t) + 0.35 * evolving(p * 2.3 + vec3(7.1), t * 1.3);
    let colour = gradient(field + aurora.colour_lift * (up - 0.3));
    // The curtain hangs far nearer than the sky dome, so depth alone would put
    // it in front of the hills; it is hidden behind them by their silhouette.
    let dir = normalize(in.world_position.xyz - view.world_position);
    // Facing is one looking straight through the sheet and nought along it.
    // Fading toward edge-on softens the line where a fold turns away, which
    // a sheet with no thickness would otherwise end on abruptly.
    let facing = abs(dot(normalize(in.world_normal), dir));
    let edge = mix(1.0, smoothstep(0.0, 0.6, facing), aurora.edge_fade);
    let strength = aurora.brightness * lower_edge * fade_upward * ends * rays * edge
        * above_hills(dir);
    // Light added to whatever is behind, which is all an aurora is: the
    // material draws with Bevy's premultiplied blend, which keeps
    // `1 - alpha` of what is behind, so alpha is nought — a strength there
    // would dim the stars and the curtains behind instead of adding to them.
    return vec4(colour * strength, 0.0);
}
