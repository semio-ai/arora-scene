// The floor: exactly the theme's ground colour wherever it is lit, darkened
// where something stands between it and a shadow-casting light.
//
// A lit floor would shade differently under every rig and from every angle,
// and stop matching the sky's ground below the horizon. This one takes nothing
// from the lights but their shadows.

import bevy_pbr::render::{
    forward_io::VertexOutput,
    mesh_view_bindings as view_bindings,
    mesh_view_types,
    shadows,
};

struct Floor {
    colour: vec4<f32>,
    // How far a full shadow darkens the floor, from not at all to black.
    shadow: f32,
    _padding: vec3<f32>,
}

@group(constants::MATERIAL_BIND_GROUP) @binding(0) var<uniform> surface: Floor;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
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
    return vec4(surface.colour.rgb * mix(1.0 - surface.shadow, 1.0, lit), 1.0);
}
