#version 450

// Samples the face's block texture and darkens it by the face's shade. Opaque: no
// transparency or alpha test yet (decision 0013).

// The block textures, one layer per texture; the layout is fixed by the renderer
// (vox-render's interface/chunk.rs).
layout(set = 0, binding = 0) uniform texture2DArray block_textures;
layout(set = 0, binding = 1) uniform sampler block_sampler;

layout(location = 0) in vec2 v_uv;
layout(location = 1) flat in uint v_layer;
layout(location = 2) in float v_shade;

layout(location = 0) out vec4 out_color;

void main() {
    vec3 texel = texture(sampler2DArray(block_textures, block_sampler), vec3(v_uv, float(v_layer))).rgb;
    out_color = vec4(texel * v_shade, 1.0);
}
