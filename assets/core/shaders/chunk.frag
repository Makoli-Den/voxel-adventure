#version 450

// Samples the face's block texture, darkens it by the face's shade and fades it into the
// fog colour with the distance from the camera. Opaque: no transparency or alpha test yet
// (decision 0013).

// The same block as in chunk.vert; this stage reads the fog.
layout(push_constant) uniform Draw {
    mat4 view_projection;
    vec4 chunk_origin;
    // x: fog start, y: fog end (blocks from the camera), z: 1 / (end - start), or 0 when
    // fog is off; w: 0.
    vec4 fog;
    // rgb: the fog colour, the same as the sky; a: 1.
    vec4 fog_color;
} draw;

// The block textures, one layer per texture; the layout is fixed by the renderer
// (vox-render's interface/chunk.rs).
layout(set = 0, binding = 0) uniform texture2DArray block_textures;
layout(set = 0, binding = 1) uniform sampler block_sampler;

layout(location = 0) in vec2 v_uv;
layout(location = 1) flat in uint v_layer;
layout(location = 2) in float v_shade;
layout(location = 3) in vec3 v_offset;

layout(location = 0) out vec4 out_color;

// How much of the colour the fog replaces: smoothstep(start, end, distance), written with the
// pushed 1 / (end - start) so that fog that is off (z = 0) gives 0 without a branch. The
// distance is radial (not the view depth), so turning the camera does not move the fog.
float fog_amount(float distance) {
    float t = clamp((distance - draw.fog.x) * draw.fog.z, 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

void main() {
    vec3 texel = texture(sampler2DArray(block_textures, block_sampler), vec3(v_uv, float(v_layer))).rgb;
    vec3 color = mix(texel * v_shade, draw.fog_color.rgb, fog_amount(length(v_offset)));
    out_color = vec4(color, 1.0);
}
