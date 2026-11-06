// The range of hills along the horizon, shared by everything that has to sit
// in front of or behind it: the sky draws it, and the aurora hides behind it.

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

// One above the hills, nought on them, for a direction from the camera in this
// Y-up world. Studio's (x, y, z) is (x, -z, y) here.
fn above_hills(dir: vec3<f32>) -> f32 {
    return mountain(atan(dir.y) - MOUNTAIN_LIFT, atan2(-dir.z, dir.x));
}
