#version 450

// Chunk vertices: moves each corner from chunk-local to world blocks, places it with
// the camera matrix and colors it by block and face. Vulkan clip space: x right,
// y down, depth 0..1.

// Pushed once per chunk; the layout is fixed by the renderer (vox-render's interface/chunk.rs).
layout(push_constant) uniform Draw {
    mat4 view_projection;
    // xyz: world position of the chunk's corner (0, 0, 0) in blocks; w is 0.
    vec4 chunk_origin;
} draw;

// xyz: corner position in blocks (0..32), w: face index (order of vox_world::Face::ALL).
layout(location = 0) in uvec4 in_pos_face;
// Block id, the number a chunk stores for the block.
layout(location = 1) in uint in_block;

layout(location = 0) out vec3 v_color;

// Brightness per face: +X, -X, +Y, -Y, +Z, -Z. The top is brightest, the bottom
// darkest, and every side differs, so block edges read clearly without lighting.
const float FACE_SHADE[6] = float[6](0.80, 0.65, 1.00, 0.50, 0.90, 0.72);

// Temporary base colors, until textures are used (decision 0013). The palette is indexed
// by the registry id, which the registration order of BASE_BLOCKS in vox-game's content.rs
// fixes (1 grass, 2 dirt, 3 stone); keep the two in step. Id 0 is air and never meshed.
const vec3 PALETTE[4] = vec3[4](
    vec3(1.00, 0.00, 1.00), // air (unused)
    vec3(0.36, 0.66, 0.24), // 1: grass
    vec3(0.53, 0.37, 0.22), // 2: dirt
    vec3(0.52, 0.52, 0.54)  // 3: stone
);

// Any block without a palette entry shows up loudly.
const vec3 UNKNOWN = vec3(1.00, 0.00, 1.00);

void main() {
    gl_Position = draw.view_projection * vec4(vec3(in_pos_face.xyz) + draw.chunk_origin.xyz, 1.0);
    vec3 base = (in_block >= 1u && in_block < uint(PALETTE.length())) ? PALETTE[in_block] : UNKNOWN;
    v_color = base * FACE_SHADE[in_pos_face.w];
}
