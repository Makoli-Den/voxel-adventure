# Voxel Adventure (working title)

A voxel game: the open world, freedom of action and world generation of Minecraft, combined with the adventure, difficulty and boss fights of Terraria. The top priority is speed: the game must be extremely fast and run on old devices. Mods, texture packs and shader packs are part of the core.

> Status: very early. The game opens a window, picks a GPU through Vulkan and draws a colored triangle with shaders that are loaded from files and compiled at startup. The world, mods and real rendering are still to come.

## Stack
- A custom engine in **Rust**, graphics on **Vulkan** (`ash`). The decision and its reasons: [docs/decisions/0001-rust-vulkan-own-engine.md](docs/decisions/0001-rust-vulkan-own-engine.md)
- Windowing with `winit`
- Shaders are GLSL files compiled to SPIR-V at runtime with `naga`, so shader packs need no build tools: [docs/decisions/0003-runtime-shader-compilation.md](docs/decisions/0003-runtime-shader-compilation.md)
- Mods run in a sandbox (WASM or Lua), content is described by data
- Rules for code, architecture and documentation: [CLAUDE.md](CLAUDE.md)

## Repository layout
```
assets/
  shaders/     built-in GLSL shaders, read and compiled when the game starts
crates/
  vox-world/   blocks and chunks; knows nothing about rendering, mods or networking
  vox-render/  rendering behind the `Renderer` trait; the only crate that touches Vulkan
  vox-game/    the executable: window, event loop, wires the layers together
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
```
Formatting, the linter and the tests must pass before every commit.

## Performance principles
See [docs/performance-notes.md](docs/performance-notes.md).

## License
Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

## Contributing
Forks and collaboration are welcome. Please read [CLAUDE.md](CLAUDE.md) first: it describes the architecture and the code rules every change must follow.
