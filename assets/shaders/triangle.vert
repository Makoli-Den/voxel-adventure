#version 450

// The demo triangle. Positions and colors are hard-coded and picked by
// gl_VertexIndex, so the pipeline needs no vertex buffer.
// Vulkan clip space: x points right, y points down.

const vec2 POSITIONS[3] = vec2[3](
    vec2(0.0, -0.5),
    vec2(0.5, 0.5),
    vec2(-0.5, 0.5)
);

const vec3 COLORS[3] = vec3[3](
    vec3(1.0, 0.0, 0.0),
    vec3(0.0, 1.0, 0.0),
    vec3(0.0, 0.0, 1.0)
);

layout(location = 0) out vec3 v_color;

void main() {
    gl_Position = vec4(POSITIONS[gl_VertexIndex], 0.0, 1.0);
    v_color = COLORS[gl_VertexIndex];
}
