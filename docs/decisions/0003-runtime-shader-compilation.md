# 0003. Shaders are GLSL files compiled at runtime

Status: accepted. The shader location and loading were changed by decision 0013: the shaders are now the resources `core:shaders/*` (files in `assets/core/shaders/`), read by the game through its resource packs and passed to `create_renderer` as text (`ShaderSources`).

## Context
Shader packs are part of the core (decision 0001): a pack is a folder of shader files that replaces the built-in ones. The renderer therefore has to load shader source from disk while the game runs, not only at build time. Vulkan consumes SPIR-V, so the source has to be compiled somewhere. Players and pack authors must not need a C++ toolchain or the Vulkan SDK.

## Decision
- Shaders are written in **GLSL 450, Vulkan style**, one file per stage (`chunk.vert`, `chunk.frag`; later `overlay.vert`, `overlay.frag`).
- The built-in shaders live in `assets/shaders/`. The renderer receives the folder as a path (`create_renderer(..., shader_dir)`) and reads the files from there at startup. *(Superseded by decision 0013: the files are now in `assets/core/shaders/`, and the renderer receives their text as `ShaderSources`.)*
- They are compiled to **SPIR-V 1.0** (the version Vulkan 1.0 accepts) with the pure-Rust **`naga`** crate: the GLSL front end, the validator and the SPIR-V back end. naga is used only inside `vox-render`; no naga type appears in the public API.
- A missing or broken shader is a `RenderError::Shader` carrying the file name and the compiler message, never a panic. *(Since decision 0013 the game reports a missing shader by its resource id before the renderer starts, and `RenderError::Shader` names the shader by its resource id.)*
- The SPIR-V writer is told to bounds-check array indices and buffer accesses (`Restrict`, which clamps indices into range), because the device is created without `robustBufferAccess` and a pack shader must not be able to read or write outside its data.
- The validator allows only push constants (naga calls them "immediates"), which are core Vulkan 1.0 and need no device feature, so a module can only use core features the device always has. Widening this set requires enabling the matching device features.
- After validation, each shader is checked against the interface of its own pipeline, described in `vox-render/src/interface/` (one description per pipeline, see decision 0009): no descriptor resources (the layout has no descriptor sets) *(changed by decision 0013: a pipeline's interface may list descriptor resources, and a shader may declare only those, at their set and binding, of their kind, in their stage; the chunk pipeline lists the block textures, the overlay lists none)*, push constants only in the stage the description names and no larger than the layout's range, vertex inputs only at the locations the pipeline feeds and with their exact types, vertex input built-ins limited to vertex and instance index, and every fragment input location written by the vertex stage with the same scalar or vector type. A mismatch is a `RenderError::Shader` naming the file. So a loaded shader is guaranteed to use core capabilities only AND to match the pipeline layout and vertex input. Interpolation qualifiers and fragment outputs are not checked; neither can make the pipeline invalid.
- Until the resource virtual file system exists, the game passes the folder from the source tree (`CARGO_MANIFEST_DIR/../../assets/shaders`). Later the file system resolves shaders through the active packs. *(Done by decision 0013: the game reads `core:shaders/*` through its resource packs.)*

## Reasons
- **Files, not embedded bytes:** a pack can only replace what is loaded from a file. Reading the built-in shaders the same way means packs and the base game take one code path.
- **naga:** pure Rust, so `cargo build` is all anyone needs on every platform. It also validates the module, so a broken pack fails with a readable message pointing at the line instead of crashing the driver.
- **GLSL:** the shader language pack authors already know (Minecraft shader packs are GLSL), with the most learning material.

## Rejected
- **`shaderc` (glslang):** the reference compiler, but it builds C++ code and needs CMake and a C++ toolchain, or a prebuilt library per platform.
- **Precompiled SPIR-V checked into the repository:** hides the source from pack authors and lets the binary drift away from the source.
- **`include_bytes!` / `include_str!`:** bakes the shaders into the executable, which defeats shader packs.

## Risks
- naga's GLSL front end does not cover all of GLSL (for example some extensions and rarer built-ins). Packs must stay within what naga supports. If that becomes a real limit, a second front end can be added behind the same functions.
- Compiling at startup costs some time. For a few shaders this is milliseconds; with many shaders a cache of compiled SPIR-V keyed by source hash is the answer, once measurements show it is needed.
- naga's own clip space points Y up, so its SPIR-V writer flips Y by default. The renderer turns that off, because our GLSL is written for Vulkan clip space.
