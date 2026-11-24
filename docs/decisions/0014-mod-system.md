# 0014. The mod system: packages, load order, Lua sandbox and API v1

Status: accepted.

## Context
Architecture rules 1, 3, 6 and 7 say: the base game is a mod on top of a narrow core, content is data, mods run in a sandbox with no file or network access unless granted, and the mod API is versioned with explicit dependencies between mods. Today there are no mods. What exists:
- `vox-resources` (decision 0013): `ResourceId` (`namespace:path`), `DirPack` (a directory, `ns:a/b` is `<root>/ns/a/b`), `PackStack` (whole-file override, highest pack wins). The game stacks the built-in `assets/` (namespaces `core` and `base`) and every `--pack`.
- `vox-content`: `load_blocks_from(stack, file)` already names registered blocks after the namespace of the file (`mymod:blocks.json` registers `mymod:<name>`), but builds a fresh `BlockRegistry` per call and `load_content` reads only `base:blocks.json`.
- `vox-world`: `World` of loaded chunks owned by the main thread, `set_block` returning the chunks to remesh, `BlockRegistry` mapping names to `BlockId`.
- `vox-game` (decision 0011): player edits run on the main thread in `App::edit`, which writes the block and remeshes the changed chunks; streaming workers generate and mesh self-contained jobs and never read the shared world (decision 0010).
- `serde`, `serde_json` and `semver` are already in `Cargo.lock`; `cc` too (through `android-activity` and `wayland-backend`).

## Decision

### (a) A mod is a pack plus a manifest and code
**Layout.** A mod is a directory:
```
mods/example/
  mod.json                                manifest, outside the resource namespace
  assets/                                 a pack, laid out exactly like the built-in assets/
    example/blocks.json                   example:blocks.json, registers example:<name>
    example/textures/block/ruby.png
    example/scripts/main.lua              the entry script
    base/textures/block/stone.png         a mod may override any resource, like any pack
```
`assets/` is a `DirPack`; nothing in the pack code changes. The manifest sits outside the pack, so no pack can override another mod's manifest. A resource pack (`--pack`) is the same pack without manifest and code. A data-only mod (no `entry`) is valid.

**Manifest `mod.json`** (JSON, `deny_unknown_fields`, read with a size cap `MAX_MANIFEST_BYTES`):
```json
{ "format": 1,
  "id": "example",
  "version": "0.3.1",
  "api": "1.0",
  "dependencies": { "base": "^0.1" },
  "entry": "example:scripts/main.lua",
  "capabilities": ["world.read", "world.write"] }
```
- `id`: the mod's namespace, same character rule as a `ResourceId` namespace; must equal the directory name (so a mods folder cannot hold the same id twice by accident); `core` is reserved for the engine.
- `version`: semver (`semver::Version`).
- `api`: the mod API version the mod was written against, `MAJOR.MINOR`; it means `^MAJOR.MINOR` (see (c)).
- `dependencies`: mod id to a cargo-style version requirement (`semver::VersionReq`). Required dependencies only.
- `entry`: optional resource id of the entry script; its namespace must be the mod's own id.
- `capabilities`: optional, see (c). Unknown names are a manifest error.

**Where mods are found.** Every immediate subdirectory with a `mod.json` of the built-in `mods/` folder of the source tree (baked in like `BUILT_IN_PACK`, same TODO for packaging) and of every `--mods DIR` (repeatable). The base game moves from `assets/base/` to `mods/base/` (`mods/base/mod.json`, `mods/base/assets/base/...`, version `0.1.0`, no script); `assets/` keeps only `core`.

**Load order and the pack stack.** Dependency resolution is a pure function `resolve(manifests, MOD_API_VERSION) -> Result<Vec<ModManifest>, Vec<ResolveError>>`:
1. Check each mod alone: duplicate id across folders, `api` major differs from the game's or minor is newer than the game's.
2. Check each dependency: missing mod, or present with a version outside the requirement.
3. Topological sort (Kahn) with ties broken by id, so the order is the same on every machine and run. Mods left over form cycles; the error names the mods of each cycle.

All errors are collected and reported together, and the game stops at startup, as it does for a broken blocks file: a half-loaded mod set is worse than a clear message. The pack stack becomes, bottom to top: `assets/` (`core`), each mod's `assets/` in load order (a mod overrides its dependencies), then every `--pack` (the player's texture packs win over mods). Blocks are registered per mod in load order from `<id>:blocks.json` if the mod has one, into one registry. Ids therefore depend on the mod set; this was already true (decision 0013) and is solved by the name-to-id table of world saves, not here.

### (b) Sandbox: Lua 5.4 through `mlua`, vendored
**Decision:** mods are scripted in **Lua 5.4**, embedded with **`mlua`** (features `lua54`, `vendored`; no `send`, no `async`, no `serialize`). The exact version is pinned in S4.

| | Lua 5.4 (`mlua`, vendored) | `wasmi` | `wasmtime` |
|---|---|---|---|
| Footprint | about 300 KB of code, a VM costs tens of KB | pure Rust interpreter, moderate tree | JIT (Cranelift), many crates, several MB |
| Old devices | interpreter, runs everywhere C runs, no JIT | interpreter, everywhere | JIT, executable memory, not allowed on some platforms |
| Speed | fast interpreter, cheap calls | interpreter of similar speed, plus marshalling | fastest |
| Limits | count hook (instructions) and memory cap built in | fuel and memory limits built in | fuel/epochs and memory limits |
| Modder cost | edit a text file, no tools; the language voxel modders already know (Luanti/Minetest) | a compiler toolchain per language | same as `wasmi` |
| Host API cost | tables, strings and functions map directly | an ABI over linear memory to design and maintain | same as `wasmi` |
| Windows build | `cc` compiles about 30 C files with the MSVC compiler the Rust MSVC toolchain already requires; no CMake | pure Rust | pure Rust, long compile |

Reasons: the first mods are small event handlers, where call overhead and memory matter more than raw throughput; Lua is tiny, has no JIT to forbid, and needs no host ABI design. Determinism: bit-exact replay across machines is not required, because mods run only where the world is authoritative (single player now, the server later); clients never run mod code to simulate. Lua's randomized string hashing (`pairs` order) and `math.random` are therefore acceptable and documented.

**Sandbox rules** (one `Lua` state per mod):
- Libraries: base, `string`, `table`, `math`, `utf8`. Not loaded: `io`, `os`, `package` (so no `require`), `debug`, `coroutine`. Removed from base: `dofile`, `loadfile`, `load`, `collectgarbage`; removed from `string`: `dump`. `print` is replaced by a host function that queues the line with the mod id.
- Code is loaded as text only (`ChunkMode::Text`): precompiled bytecode is the classic way out of a Lua sandbox.
- The only way to reach the game is the `vox` table and the handles the host passes to handlers. There is no file or network API at all in v1.
- No `unsafe` in our code: `mlua`'s safe constructor covers these libraries.

**Limits:**
- Instructions: a count hook every `HOOK_INTERVAL` instructions adds to the current call's count; above `MAX_INSTRUCTIONS_PER_CALL` (handlers) or `MAX_INSTRUCTIONS_AT_LOAD` (the entry script) it raises an error. Exhaustion is **sticky**: once over budget, every later hook check in that call raises again, and the host checks the flag after the call and treats it as a failure even if the script caught the error with `pcall`. Outside a host call the budget is zero, so a `__gc` finalizer that runs between calls is stopped at the first check.
- Memory: `Lua::set_memory_limit(MAX_MOD_MEMORY)` per state.
- Writes: at most `MAX_EDITS_PER_CALL` block writes per handler call; more is an error.
- Log: at most `MAX_LOG_LINES_PER_CALL` lines per call; the rest are counted and dropped.
- Provisional values, to be fixed by measurement in S7 on the low-end device: `HOOK_INTERVAL = 1_000`, `MAX_INSTRUCTIONS_PER_CALL = 200_000` (about 4 ms at an assumed 50 M Lua instructions per second on an old CPU, a quarter of a 60 Hz frame), `MAX_INSTRUCTIONS_AT_LOAD = 10_000_000`, `MAX_MOD_MEMORY = 16 MiB`, `MAX_EDITS_PER_CALL = 64`, `MAX_LOG_LINES_PER_CALL = 16`.

**Error isolation:** any Lua error (runtime error, syntax error, budget, memory, too many writes, missing capability) disables that mod for the rest of the session: its state is dropped, its buffered writes of that call are discarded, and a warning with the mod id and the Lua traceback is returned to the game, which prints it. Other mods and the game continue. Mods that depend on a disabled mod keep running (in v1 mods cannot call each other). Its data (blocks, textures) stays registered: data is loaded before scripts and is not undone.

### (c) Mod API v1
**Versioning.** `MOD_API_VERSION` (a `semver::Version`, starting at `1.0.0`) lives in `vox-mods`. Adding a function, event or event field is a minor bump; removing or changing anything is a major bump. The game supports exactly one major version. A mod declaring `"api": "1.2"` loads on 1.2 and later 1.x, and is refused on 1.1 and on 2.x with a message naming both versions. The API reference is `docs/mod-api.md`, updated in the same PR as every API change.

**What a mod does in v1:**
- Data (no code): blocks and textures through `<id>:blocks.json` and its pack, as the base game does; overrides of any resource.
- Code: the entry script runs once at startup, after all blocks are registered. It registers handlers:
  ```lua
  vox.on("block_broken", function(event, world)
    -- event = { x = 10, y = 4, z = -3, block = "base:stone" }  the block that was there
    if event.block == "base:stone" and world:get_block(event.x, event.y + 1, event.z) == "core:air" then
      world:set_block(event.x, event.y + 1, event.z, "base:dirt")
    end
  end)
  ```
  Events: `block_broken` (by the player; `block` is the old block) and `block_placed` (by the player; `block` is the new block). An unknown event name or a second handler for the same event from one mod is an error at load.
- `vox.api_version` (string), `print` (queued, prefixed with the mod id).
- **World handle** (valid only during the handler call): `world:get_block(x, y, z)` returns the block name, or `nil` if the chunk is not loaded; `world:set_block(x, y, z, name)` queues a write and returns `false` if the chunk is not loaded. An unknown block name is an error. Coordinates are integers in world blocks. Blocks are named, not numbered, in v1: names are stable across mod sets and events are rare.

**Semantics:** handlers are notifications after the edit; they cannot cancel it. Every mod with a handler is called in load order and sees the same world (the state right after the player's edit); its writes are buffered and applied after all mods returned, in load order, so a later mod wins on the same cell. Writes by mods do not raise events (no recursion). All changed chunks are remeshed once.

**Permissions.** Capabilities are declared in the manifest. v1 has two, both about the game world only: `world.read` (`get_block`) and `world.write` (`set_block`). Calling a function whose capability was not declared is an error (the mod is disabled). World capabilities are granted by declaring them, because they cannot reach the player's machine; the game prints each mod's capabilities at startup. **Sensitive** capabilities (files, network, native code) need an explicit grant by the player that no pack or mod can give itself; the first such capability brings the grant mechanism (`--grant MOD:CAPABILITY` until there is a settings screen) in the same PR. None exists in v1.

### (d) Crates and threads
| crate | new? | knows | contains |
|---|---|---|---|
| `vox-mods` | new | `vox-resources`, `vox-world`, `serde`, `serde_json`, `semver`, `mlua` | manifest, discovery, resolution, sandboxed VMs, the API, `ModHost` |
| `vox-content` | changed | as today | blocks of several namespaces into one registry |
| `vox-game` | changed | all | `--mods`, wiring, event dispatch |

`vox-mods` knows nothing of Vulkan or `vox-render`, and `vox-content` does not depend on it: the game passes the namespaces in load order. Modules of `vox-mods`: `manifest.rs`, `discover.rs` (file system), `resolve.rs` (pure), `sandbox.rs` (state setup, limits), `api.rs` (`vox` table, world handle), `host.rs` (`ModHost`: all running mods, dispatch, disabling).

Interface of the host (shape, not final names):
```rust
pub struct BlockEvent { pub kind: BlockEventKind, pub position: [i32; 3], pub block: BlockId }
pub struct BlockEdit { pub position: [i32; 3], pub block: BlockId }
impl ModHost {
    pub fn start(mods: &[ModManifest], stack: &PackStack, registry: &BlockRegistry) -> (Self, Vec<ModWarning>);
    pub fn block_event(&mut self, event: &BlockEvent, world: &World, registry: &BlockRegistry) -> Vec<BlockEdit>;
    pub fn take_messages(&mut self) -> Vec<ModMessage>; // log lines and warnings for the game to print
}
```
Handlers borrow `&World` only for the call (`mlua`'s scoped userdata); writes come back as plain `BlockEdit`s that the game applies through its existing `set_block` plus remesh path. `ModHost` does not touch the world mutably, which keeps the borrow simple and the host testable with a `World` built in code.

**Threads:** `ModHost` lives on the main thread (it is not `Send`) and runs inside `App::edit`, after the player's write. Streaming workers never run mod code and terrain generation stays a pure function of `(seed, pos)` (decisions 0006 and 0010). A frame with no event and no handler costs nothing: dispatch checks whether any mod registered the event.

### (e) Steps
Each step is one PR, keeps the game working, and passes `cargo fmt`, `cargo clippy -- -D warnings` and `cargo test`. Constants get names and doc comments.

```
S1 (vox-mods: manifest, resolve) --.
S2 (vox-content: many namespaces) -+-> S3 (base as a mod, game wiring) --.
S1 ------> S4 (vox-mods: Lua sandbox, library only) ---------------------+-> S5 (events, game) -> S6 (capabilities) -> S7 (measure)
```
S1 and S2 run in parallel (different crates). S4 runs in parallel with S3 (S4 is `vox-mods` only, S3 is `vox-game`, `vox-content` and the assets move). S5 and S6 edit `vox-mods` and `vox-game`, so one after the other. One writer owns `vox-mods` from S1 to S6 (a new `mods-dev` role is recommended; until then `world-dev`), with the strongest model for S4.

**S1. Manifest and dependency resolution** (`crates/vox-mods/`: `lib.rs`, `manifest.rs`, `discover.rs`, `resolve.rs`, `error.rs`, fixtures under `tests/fixtures/`). No scripting, no `mlua` yet, not used by the game. Done: tests for manifest parsing (valid, unknown key, bad id, id differs from the folder, reserved `core`, bad version, bad requirement, entry outside the mod's namespace, unknown capability, oversized file); resolution (chain order, ties by id, missing dependency, version mismatch, two cycles reported together, api major and minor mismatch, duplicate id across two folders, all errors collected at once); the order is identical over shuffled input.

**S2. Blocks from several namespaces** (`vox-content` only). `load_content(stack, namespaces)` registers `<ns>:blocks.json` of each namespace that has one, in order, into one registry; the texture layers are shared across all files; a missing file for a namespace is skipped, a broken one is an error naming the file. The game passes `["base"]` until S3. Done: tests with two namespaces (ids follow the order, a texture used by both gets one layer, a duplicate name inside one file is an error, the same local name in two namespaces is fine); `base` alone gives ids 1, 2, 3 as before.

**S3. The base game becomes a mod** (`vox-game`: `main.rs`, `packs.rs`, `cli.rs`, `content.rs`; move `assets/base/` to `mods/base/assets/base/`, add `mods/base/mod.json`; README layout and options). `--mods DIR` (repeatable). Startup: discover, resolve (errors stop the game, listed), build the pack stack as in (a), load content for the namespaces in load order, print the load order. Done: the game looks exactly as before; a test fixture data-only mod depending on `base` adds a block whose name and textures load; a missing dependency and a cycle stop the game with all errors listed (CLI-level tests, no window); one window check that the world is unchanged.

**S4. The Lua sandbox** (`vox-mods`: `sandbox.rs`, `api.rs` with `vox.on`, `vox.api_version` and `print` only, `host.rs` with `ModHost::start` and `take_messages`; adds `mlua`). Not wired into the game. Done: tests that `io`, `os`, `debug`, `package`, `require`, `load`, `loadfile`, `dofile`, `collectgarbage`, `string.dump` and `coroutine` are absent; a precompiled chunk is refused; an endless loop at load and in a handler is stopped and disables the mod; an endless loop inside `pcall` still disables it (sticky budget); a memory bomb hits the cap and disables the mod; a syntax error disables only that mod and the next one still loads; a hello-world fixture mod's `print` arrives in `take_messages` with its id; the log cap holds. Recorded in `docs/benchmarks.md`: clean release build time and executable size before and after on Windows MSVC, and memory of an empty mod state. A fresh clone builds on Windows with only the Rust MSVC prerequisites installed.

**S5. Block events and the world handle** (`vox-mods`: `api.rs`, `host.rs`; `vox-game`: `main.rs` (`App::edit`), `editing.rs`; `docs/mod-api.md`; an example mod under `examples/mods/`). `ModHost::block_event`, the world handle, buffered writes, `MAX_EDITS_PER_CALL`; the game starts the host after content, dispatches `block_broken`/`block_placed` after the player's write, applies the returned edits and remeshes the union of changed chunks once. Done: host tests with a `World` built in code (reads see the world after the player's edit, writes return as edits in load order, a later mod wins, unloaded chunk gives `nil`/`false`, an unknown block name and too many writes disable the mod, a handle kept past its call is unusable); a game-side test that the applied edits remesh each chunk once; one window check with `--mods examples/mods` where breaking stone makes the example mod's visible reaction appear.

**S6. Capabilities** (`vox-mods`: `manifest.rs`, `api.rs`, `host.rs`; `vox-game` prints the capabilities at startup; `docs/mod-api.md`). Done: tests that `get_block` without `world.read` and `set_block` without `world.write` disable the mod with a message naming the capability; declared capabilities work; the example mod and fixtures declare theirs.

**S7. Measure** (`tester`, `docs/benchmarks.md` and the constants' doc comments). On the development machine and the low-end device: cost of one handler call with an empty and a typical handler, Lua instructions per second (to fix `MAX_INSTRUCTIONS_PER_CALL`), memory per mod state, cost of applying `MAX_EDITS_PER_CALL` scattered writes including remeshing. Done: every provisional constant carries its measured reason.

## Not built yet
Each needs its own decision when a real mod needs it: a tick or timer event (needs a fixed simulation tick first), hooks into terrain generation, cancellable events, per-block scripted behaviour, items, entities, crafting and inventory APIs, calls and shared state between mods, `require` and multi-file scripts, file, storage and network capabilities with the grant mechanism, trusted native mods, a WASM backend, hot reload of scripts, zip or downloaded mods and signing, saving mod state, running mods on a server or syncing them to clients, optional dependencies, conflicts and load-before hints, a mod list or settings screen, numeric block ids in the API.

## Reasons
- **A mod is a pack:** the resource system, overrides and namespaces of decision 0013 are reused unchanged; texture packs, data mods and scripted mods are one concept with optional parts.
- **Manifest outside the pack:** it must not be overridable, and it is read before the stack exists.
- **Stop on resolution errors, disable on script errors:** resolution errors are found before anything runs and the player can fix them; script errors happen while playing and must not end the game.
- **One Lua state per mod:** per-mod memory cap, budget and failure without shared globals; a state costs tens of KB.
- **Buffered writes and shared pre-dispatch view:** no `&mut World` inside Lua, deterministic result independent of how mods interleave reads, and the existing remesh path does the work.
- **Names in the API:** the registry already maps them; numbers would leak ids that change with the mod set.

## Rejected
- **WASM first (`wasmtime`):** JIT and size work against old devices and some platforms; compile time. **`wasmi`:** the strongest alternative (pure Rust, deterministic, fuel), rejected for v1 because every modder needs a compiler toolchain and we would design and maintain a memory ABI for strings and tables. Reconsider as a second backend when a mod needs heavy computation (pathfinding, generation). **`tinywasm`:** young.
- **LuaJIT:** JIT forbidden on some platforms, FFI is a sandbox hole to keep closed, Lua 5.1, harder Windows build. **Luau:** good sandbox and a faster interpreter, but a C++ build and a dialect; it is the fallback if Lua 5.4's sandbox or speed falls short (in `mlua` mostly a feature switch plus interrupts instead of count hooks). **Pure-Rust Lua** (`piccolo` and others): immature. **`rhai`:** pure Rust with operation limits, but a slow tree-walking interpreter and a language modders do not know.
- **One VM for all mods:** shared globals, one memory cap, one failure stops everything.
- **Manifest as `<id>:mod.json` inside the pack, or TOML:** overridable by other packs; TOML as in decision 0013.
- **Ignoring a mod with missing dependencies and continuing:** silently changes the content the player asked for.
- **Writes applied immediately inside handlers:** needs `&mut World` across the Lua boundary and makes mod order visible to reads.
- **Granting world capabilities by the player:** asks about things that cannot harm the machine; the prompt would be clicked through and lose its meaning for the sensitive ones.

## Risks
- **C in the build:** `cc` needs the MSVC C compiler on Windows (part of the C++ build tools the Rust MSVC toolchain asks for) and clang from the NDK for Android. S4 verifies a fresh Windows build.
- **Lua and `mlua` are C and `unsafe` internally**; a bug there is a memory-safety hole. Text-only loading and no `debug` library close the known escapes; keep `mlua` updated.
- **The count hook does not see time inside C library functions** (`string.rep`, `string.gsub`, `table.concat`, patterns). The memory cap bounds the inputs; if S7 or a report shows long calls, the hook also checks a wall-clock deadline.
- **Not deterministic** (`pairs` order, `math.random`): fine while mods run only on the authoritative side; lockstep networking would need another choice.
- **Remesh after mod writes** can cost several milliseconds (decision 0011's unmeasured remesh, times up to `MAX_EDITS_PER_CALL` scattered writes); S7 measures it.
- **Block ids follow the mod set**, and the 256 texture layers are shared by all mods (decision 0013 risks).
- **Events are player-only and single-player**; a server and several players will change their shape, which is a major API bump.
- **Moving `assets/base/` to `mods/base/`** changes only the source tree: packs address resources by id, so existing `--pack` packs keep working.

## Answers to the open questions
- No new agent role: `world-dev` owns `vox-mods`; the Lua sandbox step (S4) runs on the strongest model and gets an independent security review.
- Mods need not be deterministic; they run only on the side that owns the world.
