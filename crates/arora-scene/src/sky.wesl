// The sky: a gradient from the horizon up to the zenith, the ground colour
// below the horizon, a range of mountains in that same colour along it, and at
// night a scatter of stars.
//
// Drawn on the inside of a dome that rides with the camera. The colour comes
// from the direction the fragment lies in from the camera, never from where on
// the dome it is, so the dome's size and turn are invisible.

import bevy_pbr::render::{forward_io::VertexOutput, mesh_view_bindings::view};

struct Sky {
    zenith: vec4<f32>,
    horizon: vec4<f32>,
    ground: vec4<f32>,
    // How strongly stars show, from none to full.
    stars: f32,
    // The height of a unit direction at which the sky is halfway from the
    // horizon colour to the zenith's.
    midpoint: f32,
    _padding: vec2<f32>,
}

@group(constants::MATERIAL_BIND_GROUP) @binding(0) var<uniform> sky: Sky;

fn hash(p: vec3<f32>) -> f32 {
    let q = fract(p * 0.3183099 + vec3(0.71, 0.113, 0.419));
    let r = q * 17.0;
    return fract(r.x * r.y * r.z * (r.x + r.y + r.z));
}

// A star field over the unit sphere: the direction is scaled into a lattice,
// and a few of the lattice's cells hold one star each, at a jittered point.
// Distance to that point in the lattice is what gives a star its size.
fn star_field(dir: vec3<f32>) -> f32 {
    let p = dir * 260.0;
    let cell = floor(p);
    if hash(cell + 13.0) < 0.985 {
        return 0.0;
    }
    let point = cell + vec3(hash(cell), hash(cell + 3.0), hash(cell + 7.0));
    let brightness = mix(0.35, 1.0, hash(cell + 21.0));
    return brightness * smoothstep(0.14, 0.0, distance(p, point));
}

// One-dimensional value noise, as Studio's background shader has it.
fn noise_1d(x: f32) -> f32 {
    let i = floor(x);
    let f = fract(x);
    let a = fract(sin(i) * 43758.5453123);
    let b = fract(sin(i + 1.0) * 43758.5453123);
    return mix(a, b, smoothstep(0.0, 1.0, f));
}

// One above the mountains, nought on them: Studio's silhouette, unchanged but
// for no longer drifting with time. Its angles are Studio's, a Z-up world's —
// `latitude` rises with height and `longitude` turns about the vertical — and
// the caller supplies them from this Y-up one.
fn mountain(latitude: f32, longitude: f32) -> f32 {
    let base = latitude + 0.55 + 0.2 * mix(-0.85, -0.1, sin(longitude) * 0.5 + 0.5);
    let noise1 = noise_1d(3.0 * cos(longitude * 2.0)) / 2.0;
    let noise2 = noise_1d(5.0 * cos(longitude * 3.0)) / 2.0;
    return smoothstep(0.495, 0.505, 0.75 * base + 0.18 * noise1 + 0.12 * noise2);
}

// How far the range is raised above the horizon, in radians. Unraised, its
// valleys dip below the horizon and the sky meets the floor directly between
// the peaks; raised this far, the lowest valley sits clear of it and the
// range reads as a continuous wall of hills, as it does in Studio.
const MOUNTAIN_LIFT: f32 = 0.1;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let dir = normalize(in.world_position.xyz - view.world_position);
    if dir.y < 0.0 {
        return vec4(sky.ground.rgb, 1.0);
    }
    // An exponent chosen so that `t` is exactly one half at the midpoint.
    let midpoint = clamp(sky.midpoint, 1e-4, 0.9999);
    let t = pow(dir.y, log(0.5) / log(midpoint));
    var colour = mix(sky.horizon.rgb, sky.zenith.rgb, t);
    // Stars thin out toward the horizon, as haze would take them.
    colour += vec3(star_field(dir)) * sky.stars * smoothstep(0.0, 0.25, dir.y);
    // Studio's (x, y, z) is (x, -z, y) here. The mountains are the ground's
    // colour, so they read as the floor rising into hills, and they stand in
    // front of the stars.
    let sky_over_hills = mountain(atan(dir.y) - MOUNTAIN_LIFT, atan2(-dir.z, dir.x));
    colour = mix(sky.ground.rgb, colour, sky_over_hills);
    return vec4(colour, 1.0);
}
