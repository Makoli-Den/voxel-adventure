#version 450

// Writes the color interpolated across the triangle from its corners.

layout(location = 0) in vec3 v_color;

layout(location = 0) out vec4 out_color;

void main() {
    out_color = vec4(v_color, 1.0);
}
