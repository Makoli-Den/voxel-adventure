#version 450

// Chunk vertices: places each corner relative to the camera with the camera matrix, derives
// the texture coordinates from the position and hands the offset from the camera to the
// fragment shader for the fog. Vulkan clip space: x right, y down, depth 0..1.

// The layout is fixed by the renderer (vox-render's interface/chunk.rs). Everything is relative
// to the camera, so the numbers stay small far from the world origin. The matrix and the fog
// are pushed once per frame, the origin once per chunk.
layout(push_constant) uniform Draw {
    // The camera's view-projection without its translation: the camera is at (0, 0, 0).
    mat4 view_projection;
    // xyz: the chunk's corner (0, 0, 0) relative to the camera, in blocks; w: the cell size
    // in blocks (always 1.0 for now, unused here).
    vec4 chunk_origin;
    // x: fog start, y: fog end (blocks from the camera), z: 1 / (end - start), or 0 when
    // fog is off; w: 0. Used by the fragment shader.
    vec4 fog;
    // rgb: the fog colour, the same as the sky; a: 1. Used by the fragment shader.
    vec4 fog_color;
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
// The corner relative to the camera in blocks; its length is the distance the fog fades by.
layout(location = 3) out vec3 v_offset;

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
    vec3 offset = local + draw.chunk_origin.xyz;
    gl_Position = draw.view_projection * vec4(offset, 1.0);
    v_offset = offset;
    v_uv = vec2(dot(local, FACE_U[face]), dot(local, FACE_V[face]));
    v_layer = in_layer;
    v_shade = FACE_SHADE[face];
}
