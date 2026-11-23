#version 450

// Overlay rectangles: maps window pixels (from the top-left corner, y down) to Vulkan
// clip space (x right, y down, both -1..1) and passes the color on. The layout is fixed
// by the renderer (vox-render's interface/overlay.rs).

// Pushed once per frame: the size of the image being drawn, in pixels.
layout(push_constant) uniform Screen {
    vec2 window_size;
} screen;

// Corner position in window pixels.
layout(location = 0) in vec2 in_position;
// Color, red, green, blue and alpha in 0..1 (stored as 8-bit normalized values).
layout(location = 1) in vec4 in_color;

layout(location = 0) out vec4 v_color;

void main() {
    // Pixel 0 maps to -1 and the full size to +1. Clip y points down like pixel y,
    // so nothing is flipped.
    vec2 clip = in_position / screen.window_size * 2.0 - 1.0;
    // Depth 0 lies inside the 0..1 depth range; the overlay pipeline does not test depth.
    gl_Position = vec4(clip, 0.0, 1.0);
    v_color = in_color;
}
