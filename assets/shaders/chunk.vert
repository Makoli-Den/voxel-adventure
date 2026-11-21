#version 450

// Chunk vertices: places each corner with the camera matrix and colors it by
// block and face. Vulkan clip space: x right, y down, depth 0..1.

layout(push_constant) uniform Camera {
    mat4 view_projection;
} camera;

// xyz: corner position in blocks (0..32), w: face index (order of vox_world::Face::ALL).
layout(location = 0) in uvec4 in_pos_face;
// Block id, the number a chunk stores for the block.
layout(location = 1) in uint in_block;

layout(location = 0) out vec3 v_color;

// Brightness per face: +X, -X, +Y, -Y, +Z, -Z. The top is brightest, the bottom
// darkest, and every side differs, so block edges read clearly without lighting.
const float FACE_SHADE[6] = float[6](0.80, 0.65, 1.00, 0.50, 0.90, 0.72);

// Temporary base colors for block ids 0..3, until textures and the block registry
// exist. The ids must match the ones the demo scene uses (vox-game's demo module:
// 1 grass, 2 dirt, 3 stone); keep them in step with vox-world's terrain ids when
// those appear. Id 0 is air and never meshed.
const vec3 PALETTE[4] = vec3[4](
    vec3(1.00, 0.00, 1.00), // air (unused)
    vec3(0.36, 0.66, 0.24), // 1: grass
    vec3(0.53, 0.37, 0.22), // 2: dirt
    vec3(0.52, 0.52, 0.54)  // 3: stone
);

// Any block without a palette entry shows up loudly.
const vec3 UNKNOWN = vec3(1.00, 0.00, 1.00);

void main() {
    gl_Position = camera.view_projection * vec4(vec3(in_pos_face.xyz), 1.0);
    vec3 base = (in_block >= 1u && in_block <= 3u) ? PALETTE[in_block] : UNKNOWN;
    v_color = base * FACE_SHADE[in_pos_face.w];
}
