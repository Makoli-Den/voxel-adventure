# 0001. A custom engine in Rust and Vulkan

Status: accepted.

## Context
We need a voxel game where speed is the top priority and mods, texture packs and shader packs are built into the core. Shader packs must resemble Minecraft's: a pack describes the whole frame pipeline and is loaded from files at runtime. The game must run on old devices.

## Decision
- A custom engine.
- Language: Rust.
- Graphics: Vulkan through `ash` (a thin wrapper, full control), with rendering hidden behind a narrow interface.
- Mods: a sandbox (WASM or Lua) plus data. Native code only as an explicit "trusted mod" mode.

## Reasons
- **Unity rejected.** It does not compile shaders from source in a built game, so file-based shader packs are impossible, and loading mod code is limited to the Mono backend.
- **Godot** can load shaders at runtime, but at maximum rendering speed we would have to bypass its scene system and write directly to `RenderingDevice`, which means writing the renderer ourselves anyway.
- **Rust** gives C++-level speed, memory safety and mature sandboxing libraries (`wasmtime`, `mlua`).
- **`ash`** instead of `wgpu` or `vulkano`: we want full control over synchronization and memory for GPU-driven rendering (indirect draw, GPU culling).

## Rejected
- Unity with Burst and Jobs.
- Godot (C# or GDExtension).
- OpenGL as the main API: a lower speed ceiling and an API that is no longer evolving. It may appear as a fallback backend.
- `wgpu` as a complexity compromise.

## Risks
- Vulkan does not run on very old graphics cards. Minimum hardware requirements are not fixed yet. A rendering layer behind an interface lets us add a fallback backend later.
- Vulkan through `ash` takes a lot of code. The first goal is a vertical slice: a window, a chunk, a mesh, a shader from a file, a texture from a folder.
