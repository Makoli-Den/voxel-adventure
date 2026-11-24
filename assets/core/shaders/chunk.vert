#version 450

// Chunk vertices: moves each corner from chunk-local to world blocks, places it with
// the camera matrix and derives the texture coordinates from the position. Vulkan clip
// space: x right, y down, depth 0..1.

// Pushed once per chunk; the layout is fixed by the renderer (vox-render's interface/chunk.rs).
layout(push_constant) uniform Draw {
    mat4 view_projection;
    // xyz: world position of the chunk's corner (0, 0, 0) in blocks; w is 0.
    vec4 chunk_origin;
} draw;

// xyz: corner position in blocks (0..32), w: face index (order of vox_world::Face::ALL).
layout(location = 0) in uvec4 in_pos_face;
// Layer of the block texture array this face is drawn with.
layout(location = 1) in uint in_layer;

// Texture coordinates in blocks: one texture repeat per block (the sampler repeats).
layout(location = 0) out vec2 v_uv;
// The same on every corner of a quad, so it is not interpolated.
layout(location = 1) flat out uint v_layer;
layout(location = 2) out float v_shade;

// Per face (+X, -X, +Y, -Y, +Z, -Z): brightness, then the two tangent axes the chunk-local
// position is projected on to get u and v.
//
// The top is brightest, the bottom darkest, and every side differs, so block edges read
// clearly without lighting.
//
// Texture u runs to the right and v downwards in the picture. Seen from outside, facing the
// face (looking along -normal; the world is right-handed with +Y up):
//   face  u axis  v axis  why
//   +X    -z      -y      the viewer's right is -z; v grows downwards, so sides stand upright
//   -X    +z      -y      the viewer's right is +z
//   +Y    +x      +z      seen from above with -z ahead on screen, the right is +x
//   -Y    +x      -z      seen from below with +z ahead on screen, the right is +x
//   +Z    +x      -y      the viewer's right is +x
//   -Z    -x      -y      the viewer's right is -x
// Nothing is mirrored because u x v = -normal for every face (right x down points into the
// face). Positions are chunk-local and a chunk is a whole number of blocks, so the pattern
// lines up across chunk borders; negative coordinates are fine with repeat addressing.
// vox-render's shader tests read these tables and check both properties.
const float FACE_SHADE[6] = float[6](0.80, 0.65, 1.00, 0.50, 0.90, 0.72);
const vec3 FACE_U[6] = vec3[6](
    vec3(0.0, 0.0, -1.0),
    vec3(0.0, 0.0, 1.0),
    vec3(1.0, 0.0, 0.0),
    vec3(1.0, 0.0, 0.0),
    vec3(1.0, 0.0, 0.0),
    vec3(-1.0, 0.0, 0.0)
);
const vec3 FACE_V[6] = vec3[6](
    vec3(0.0, -1.0, 0.0),
    vec3(0.0, -1.0, 0.0),
    vec3(0.0, 0.0, 1.0),
    vec3(0.0, 0.0, -1.0),
    vec3(0.0, -1.0, 0.0),
    vec3(0.0, -1.0, 0.0)
);

void main() {
    vec3 local = vec3(in_pos_face.xyz);
    uint face = in_pos_face.w;
    gl_Position = draw.view_projection * vec4(local + draw.chunk_origin.xyz, 1.0);
    v_uv = vec2(dot(local, FACE_U[face]), dot(local, FACE_V[face]));
    v_layer = in_layer;
    v_shade = FACE_SHADE[face];
}
