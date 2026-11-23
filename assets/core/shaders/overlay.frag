#version 450

// Writes the rectangle's color. It is the same on all corners, so the interpolation
// leaves it unchanged; the pipeline blends it over the scene by its alpha.

layout(location = 0) in vec4 v_color;

layout(location = 0) out vec4 out_color;

void main() {
    out_color = v_color;
}
