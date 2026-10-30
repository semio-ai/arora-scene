// The floor: the theme's ground colour, Studio's grid drawn into it, and the
// shadows that fall on both.
//
// A lit floor would shade differently under every rig and from every angle,
// and stop matching the sky's ground below the horizon. This one takes
// nothing from the lights but their shadows.
//
// The grid is drawn here rather than as a surface of its own so it can be
// antialiased as one: each family of lines keeps a constant width on screen,
// and fades out as its cells shrink toward a pixel across. Lines denser than
// that cannot be drawn faithfully — they beat against the pixel grid into
// moiré that crawls as the camera moves — so they are faded before they get
// there rather than hidden behind a distance.

import bevy_pbr::render::{
    forward_io::VertexOutput,
    mesh_view_bindings as view_bindings,
    mesh_view_types,
    shadows,
};

struct Floor {
    colour: vec4<f32>,
    minor_colour: vec4<f32>,
    major_colour: vec4<f32>,
    // How far a full shadow darkens the floor, from not at all to black.
    shadow: f32,
    // The spacing of the minor and major lines, in metres.
    cell: f32,
    section: f32,
    // Line widths, in pixels.
    minor_width: f32,
    major_width: f32,
    // Studio's fade: the grid's strength falls as (1 - distance / reach)
    // raised to this power.
    fade_distance: f32,
    fade_strength: f32,
    _padding: f32,
}

@group(constants::MATERIAL_BIND_GROUP) @binding(0) var<uniform> surface: Floor;

// The cell size on screen, in pixels, below which a family of lines is gone,
// and above which it is drawn in full.
const VANISH_PX: f32 = 2.0;
const FULL_PX: f32 = 6.0;

// How much of this pixel a family of lines spaced `spacing` metres apart
// covers, at `width` pixels wide.
fn lines(coord: vec2<f32>, spacing: f32, width: f32) -> f32 {
    let cells = coord / spacing;
    // Cells per pixel, along each axis of the floor.
    let rate = max(fwidth(cells), vec2(1e-6));
    // Distance to the nearest line on each axis, in pixels.
    let away = abs(fract(cells - 0.5) - 0.5) / rate;
    let half = width * 0.5;
    let on = vec2(1.0) - smoothstep(vec2(half - 0.5), vec2(half + 0.5), away);
    // Each axis's lines fade by their own spacing on screen, so lines running
    // away from the camera survive where those across the view have merged.
    let shown = smoothstep(vec2(VANISH_PX), vec2(FULL_PX), 1.0 / rate);
    let coverage = on * shown;
    return max(coverage.x, coverage.y);
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // Derivatives first, while every fragment of the quad is still running.
    let coord = in.world_position.xz;
    let minor = lines(coord, surface.cell, surface.minor_width);
    let major = lines(coord, surface.section, surface.major_width);

    let reach = distance(in.world_position.xyz, view_bindings::view.world_position);
    let fade = pow(1.0 - min(reach / surface.fade_distance, 1.0), surface.fade_strength);

    var colour = surface.colour.rgb;
    colour = mix(colour, surface.minor_colour.rgb, minor * fade);
    colour = mix(colour, surface.major_colour.rgb, major * fade);

    // The same view depth Bevy's own shading uses to choose a cascade.
    let view_z = dot(vec4<f32>(
        view_bindings::view.view_from_world[0].z,
        view_bindings::view.view_from_world[1].z,
        view_bindings::view.view_from_world[2].z,
        view_bindings::view.view_from_world[3].z
    ), in.world_position);
    let normal = normalize(in.world_normal);

    var lit = 1.0;
    for (var i: u32 = 0u; i < view_bindings::lights.n_directional_lights; i++) {
        let flags = view_bindings::lights.directional_lights[i].flags;
        if (flags & mesh_view_types::DIRECTIONAL_LIGHT_FLAGS_SHADOWS_ENABLED_BIT) != 0u {
            lit = min(lit, shadows::fetch_directional_shadow(i, in.world_position, normal, view_z, in.position.xy));
        }
    }
    return vec4(colour * mix(1.0 - surface.shadow, 1.0, lit), 1.0);
}
