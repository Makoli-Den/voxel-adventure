# Voxel Adventure (working title)

A voxel game: the open world, freedom of action and world generation of Minecraft, combined with the adventure, difficulty and boss fights of Terraria. The top priority is speed: the game must be extremely fast and run on old devices. Mods, texture packs and shader packs are part of the core.

> Status: very early. The game opens a window, picks a GPU through Vulkan, streams a circle of hilly terrain chunks around the camera (generated and meshed on worker threads, faces between chunks hidden, chunks outside the view skipped), draws it textured (with mipmaps, so distant terrain does not shimmer) and lets you fly or walk over it (depth buffer, per-face shading), with block definitions, textures and shaders read through layered resource packs at startup (shaders compiled then). Mods are still to come.

## Stack
- A custom engine in **Rust**, graphics on **Vulkan** (`ash`). The decision and its reasons: [docs/decisions/0001-rust-vulkan-own-engine.md](docs/decisions/0001-rust-vulkan-own-engine.md)
- Windowing with `winit`
- Shaders are GLSL files compiled to SPIR-V at runtime with `naga`, so shader packs need no build tools: [docs/decisions/0003-runtime-shader-compilation.md](docs/decisions/0003-runtime-shader-compilation.md)
- Mods run in a sandbox (WASM or Lua), content is described by data
- Rules for code, architecture and documentation: [CLAUDE.md](CLAUDE.md)

## Repository layout
```
assets/          the engine's own resource pack; a file `assets/<namespace>/<path>` is the resource `namespace:path`
  core/shaders/  built-in GLSL shaders (`core:shaders/chunk.vert`, `chunk.frag`, `overlay.vert`, `overlay.frag`), read and compiled when the game starts
mods/            the built-in mods folder; every mod is a folder `mods/<id>/` with a `mod.json` manifest and an `assets/` pack laid out like `assets/`
  base/mod.json  the manifest of the base game, which is itself a mod
  base/assets/base/blocks.json  the base game's block definitions (`base:blocks.json`), read when the game starts
  base/assets/base/textures/block/  16x16 PNG textures of the base blocks (placeholders), decoded when the game starts and drawn on the terrain
crates/
  vox-world/     blocks and the block registry, chunks, the world of chunks, meshing and demo terrain; knows nothing about rendering, mods or networking
  vox-resources/ virtual resource file system: `namespace:path` ids and layered packs; no dependencies
  vox-content/   content read from resource packs: the blocks file and block textures (PNG decoding, tile size, the generated missing texture); depends on `vox-resources`, `vox-world`, `serde`, `serde_json`, `png`
  vox-mods/      the mod system: the `mod.json` manifest, finding mods in a folder, dependency resolution into a load order, and the Lua 5.4 sandbox that runs mod scripts with instruction, time and memory limits (see docs/mod-api.md); its manifest and load-order part is used by the game at startup, the scripts are not run yet; depends on `vox-resources`, `serde`, `serde_json`, `semver`, `memchr`, `mlua` (Lua compiled from source with the C compiler of the Rust MSVC toolchain)
  vox-render/    rendering behind the `Renderer` trait; the only crate that touches Vulkan
  vox-game/      the executable: window, event loop, wires the layers together
docs/
  decisions/   records of decisions taken
  performance-notes.md
```
Crates for mods and networking will appear once there is real code for them.

## Commands
```powershell
cargo run -p vox-game       # run the game
cargo test --workspace      # tests
cargo fmt --all --check     # formatting check
cargo clippy --workspace --all-targets -- -D warnings   # linter with zero warnings
cargo bench -p vox-world    # benchmarks, see docs/benchmarks.md (not run in CI)
```
Formatting, the linter and the tests must pass before every commit.

## Controls
Fly over the generated terrain:

| Input | Action |
|---|---|
| W, A, S, D | move forward, left, back, right |
| Space / Shift | go up / go down |
| Mouse | look around (the cursor is hidden and captured while you play; no button needed) |
| Left mouse button | break the block under the crosshair (within a few blocks) |
| F | place the selected block on the face under the crosshair (stone at the start) |
| 1 to 9 (also on the numpad) | select the 1st to 9th placeable block (every block except air, in the order of `blocks.json`; with fewer blocks the extra keys do nothing; ignored while Ctrl, Alt, Shift or Super is held) |
| Mouse wheel | cycle through all placeable blocks, wrapping around (the only way to reach blocks past the ninth) |
| G | switch between flying and walking (walking has gravity and collision; Space jumps, Shift does nothing) |
| L | turn the frame limit on or off |
| F3 | show or hide the metrics panel |
| Esc | open the pause menu, or close it again |

The pause menu (Resume, Quit) frees the cursor and stops the game's input and the walking body (the world keeps drawing); choose with Up/Down or W/S and Enter, or click. The window closing also quits. Losing the window focus opens the menu, and gaining it back does not close it, so the click that focuses the window never breaks a block: click Resume or press Esc or Enter. A movement key held while the menu opens is forgotten, so press it again after Resume. The right mouse button is unused for now.

Edits are not saved: a chunk that streams out of range and back in is generated again (decision 0011). Blocks can only be placed in the loaded terrain layer.

Options: `--background` opens the window without taking the keyboard focus (useful for scripted checks; the cursor is not captured and no menu is shown until the window is focused for the first time), `--view-radius N` sets how many chunks are streamed around the camera (0 to 30), `--fly SECONDS` flies the camera sideways by itself and then quits (for measuring streaming), `--pack DIR` layers a resource pack laid out like `assets/` over the built-in one (repeatable, a later pack wins; for example `DIR/base/textures/block/stone.png` replaces the stone texture and `DIR/core/shaders/chunk.frag` the terrain fragment shader; a texture that is missing or cannot be used shows a magenta and black checker and prints a warning), `--mods DIR` loads the mods found in `DIR` instead of the built-in `mods/` folder (repeatable; it replaces the built-in folder, so leave out the `base` mod and the game stops; a mod id may appear in only one folder; the mods load in dependency order and their assets sit between the engine's `assets/` and the `--pack` directories; the load order is printed at startup, and a missing dependency, a cycle or a broken manifest stops the game with every problem listed), `--help` lists the options.

## Frame metrics
A translucent panel in the top-left corner of the window shows live figures over the last ten seconds, refreshed twice a second: average FPS, the 1% and 0.1% lows (the average FPS of the slowest 1% and 0.1% of the frames, so they show stutter), the frame time, the time the game spent producing a frame, the GPU time (an upper bound, measured with timestamp queries) and how many chunks and triangles are drawn, plus a `Place` line with the selected block. F3 hides it, and with it the selected block: there is no other display of it yet. A summary of the whole run is printed when the game exits. The definitions are in `crates/vox-game/src/metrics.rs` and decision 0008.

The frame limit (about twice the monitor's refresh rate, at most 240 per second) is off by default, so the figures show what the game can really do and the GPU runs flat out; press L to turn the limit on, which spares the GPU and shows how much room is left.

## Performance principles
See [docs/performance-notes.md](docs/performance-notes.md).

## License
Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

## Contributing
Forks and collaboration are welcome. Please read [CLAUDE.md](CLAUDE.md) first: it describes the architecture and the code rules every change must follow.
