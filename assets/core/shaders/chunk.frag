#version 450

// Writes the face color computed in the vertex shader. It is the same on all
// corners of a face, so the interpolation leaves it unchanged.

layout(location = 0) in vec3 v_color;

layout(location = 0) out vec4 out_color;

void main() {
    out_color = vec4(v_color, 1.0);
}
