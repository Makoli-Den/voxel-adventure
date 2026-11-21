# Project rules

A voxel game in Rust: the open world and freedom of Minecraft, the adventure, difficulty and bosses of Terraria. Everything must be very fast and run on old devices. Main goals: speed, simple code, and mods, texture packs and shader packs as part of the core.

## Language
- The repository is public: everything is written in English. This covers comments, documentation, README files, commit messages, and PR and issue descriptions.
- Identifiers in code (types, functions, variables, crates) are in English as well.

## Architecture
1. **The core is narrow.** The core contains only: world storage and chunks, generation, mesh building, rendering, the virtual resource file system, the registry, the mod loader and networking. Everything else (blocks, items, mobs, bosses, crafting) is done by mods. The base game is itself a mod layered on top of the core.
2. **Layers depend only inward.** The world core and the mod system **know nothing about Vulkan** or any graphics API. Rendering hides behind a narrow interface (a trait). Any layer can be replaced without touching the others. The same layer makes it possible to add a fallback backend for old devices without Vulkan.
3. **Content is data.** Blocks, items, recipes and biomes are described by files, not code.
4. **Resources are addressed as `mod:path`.** Packs are layered by priority and any resource can be overridden: textures, sounds, models, shaders, translations.
5. **A registry with numeric identifiers.** Chunks store numbers, names are mapped through the registry, so a world does not break when the set of mods changes.
6. **Mods run in a sandbox** (WASM or Lua) with no access to files or the network unless explicitly granted. Native code is allowed only as an explicit "trusted mod" mode.
7. **The mod API is versioned**, and mods declare explicit dependencies on each other.

## Code principles
- **KISS.** The simplest solution that works. Do not add an abstraction "for the future" until two places need it.
- **SOLID**, Rust style: a module, struct or function has one responsibility; depend on traits, not on concrete implementations; extend with new types instead of editing old ones; keep interfaces narrow.
- Readability beats cleverness. Short functions, clear names, no "smart" code.
- Name the same thing the same way everywhere.

## Speed
- Speed is the main goal, but **we optimize by measurement, not by guessing.** First simple, correct code, then a benchmark (`criterion`) and profiling, then optimization where the bottleneck was measured.
- Hot paths (chunk generation, meshes, rendering) are isolated in separate modules and marked with a `// HOT PATH:` comment explaining why the code is written that way.
- In hot paths: flat arrays instead of objects, no allocations in loops, data laid out the way the CPU and GPU read it.
- Principle: "generate less, not faster": LOD, culling of the invisible, generating only what is needed. These ideas are applied before micro-optimizations (see [docs/performance-notes.md](docs/performance-notes.md)).
- Target devices include old ones. Minimum hardware requirements are recorded in a separate document and checked on a real low-end device.

## Code quality
- Zero warnings: `cargo fmt` and `cargo clippy -- -D warnings` must pass before every commit.
- `unsafe` only in isolated modules, and every block carries a `// SAFETY:` comment with the justification.
- No `unwrap` or `expect` in library code; errors go through `Result` and dedicated error types. A panic is acceptable only on a violated internal invariant.
- Core logic is covered by tests.
- One commit is one complete thought.

## Documentation
- Every module has a `//!` comment saying why it exists and what it contains.
- Every public item has a `///` comment and, where appropriate, an example. `#![warn(missing_docs)]` is enabled.
- Comments explain **why**, not what the code already says.
- Significant decisions are recorded in `docs/decisions/`, briefly: context, decision, reasons, what was rejected.
