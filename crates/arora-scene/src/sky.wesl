// The sky: a gradient from the horizon up to the zenith, the ground colour
// below the horizon, a range of mountains in that same colour along it, and at
// night a scatter of stars.
//
// Drawn on the inside of a dome that rides with the camera. The colour comes
// from the direction the fragment lies in from the camera, never from where on
// the dome it is, so the dome's size and turn are invisible.

import bevy_pbr::render::{forward_io::VertexOutput, mesh_view_bindings::view};
import super::horizon::above_hills;

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
    // The mountains are the ground's colour, so they read as the floor rising
    // into hills, and they stand in front of the stars.
    colour = mix(sky.ground.rgb, colour, above_hills(dir));
    return vec4(colour, 1.0);
}
