# Mod API

The API a mod script sees, version **1.0** (`vox.api_version`). Decision 0014 explains the
design; this page is the reference and changes in the same PR as the API. A new function, event
or field is a minor version; removing or changing anything is a major version.

Status: the game runs mod scripts and sends block events (steps S4 and S5 of decision 0014).
Capabilities are granted by declaring them in `mod.json`; there is no grant screen yet (S6).

A complete example is `examples/mods/example_pillar`: placed stone grows into a small pillar,
and breaking stone takes the stone above it down. Run it with
`cargo run -p vox-game -- --mods mods --mods examples/mods` (`--mods` replaces the built-in
`mods/` folder, so `mods` must be named too: it holds the base game).

## The entry script

A mod with code names its entry script in `mod.json`, for example
`"entry": "example:scripts/main.lua"`, which is the file
`mods/example/assets/example/scripts/main.lua`. The script runs once at startup, in a Lua 5.4
state of its own. It is read from the mod's own folder, never through the pack stack, so no
other pack can replace it, and no folder on its path may be a link. It must be Lua source text
(precompiled bytecode is refused) of at most 1 MiB.

## Functions

### `vox.on(event, handler)`
Registers `handler` (a function) for `event`; see [Events](#events). An error, which disables
the mod:
- an unknown event name;
- a second handler for the same event;
- a call after the entry script has finished (handlers are registered at startup only).

```lua
vox.on("block_broken", function(event, world)
  print("a block was broken at", event.x, event.y, event.z)
end)
```

### `vox.api_version`
The API version the game provides, as text: `"1.0"`.

### `print(...)`
Like Lua's `print`, but the line goes to the game's console with the mod's id in front.
Values are converted as `tostring` does and joined with tabs. Limits:
- at most 16 lines per call (startup or one event); the rest are dropped and counted;
- at most 1024 bytes per line; once a line is full, later values are not converted;
- control characters except tab become spaces, so a line cannot pose as another line.

## Events

A handler is called as `handler(event, world)`.

| Event | When | `event.block` |
|---|---|---|
| `block_broken` | the player broke a block | the block that was there |
| `block_placed` | the player placed a block | the block that was placed |

`event` is a fresh table per mod: `{ x = 10, y = 4, z = -3, block = "base:stone" }`, the world
block position of the cell and a block name. Changing it does not change what other mods see.

- Handlers run **after** the player's edit and cannot cancel it.
- Every mod with a handler for the event is called, in load order (dependencies first).
- All of them see **the same world**: the state right after the player's edit. Writes are
  queued, not applied, so a mod does not see the writes of the mods before it.
- When all mods have returned, the queued writes are applied in load order, each mod's in the
  order it made them, so **a later mod wins** on the same cell.
- Writes by mods **raise no events**, so mods never call each other through the world.
- Every chunk the edit and the writes changed is rebuilt once.

## The world handle

`world` is valid **only during the handler call**. A handle kept in a variable and used in a
later call raises an error (`a destructed callback ... was called`).

### `world:get_block(x, y, z)`
The name of the block at the world block position, such as `"core:air"` or `"base:stone"`, or
`nil` if that chunk is not loaded (the world is only loaded around the player, and only a
layer of it vertically). Needs the capability `world.read`.

### `world:set_block(x, y, z, name)`
Queues writing the block `name` at the position (see [Events](#events) for when it is
applied). Returns `true`, or `false` if the chunk is not loaded (nothing is queued). Needs the
capability `world.write`.

### Arguments
Call both with a colon (`world:get_block`). Coordinates are integers from -2147483648 to
2147483647 (`2.0` is accepted, `2.5` and `"2"` are not). Blocks are named, not numbered: names
stay the same when the set of mods changes, numbers do not. A wrong argument is an ordinary
error, which `pcall` can catch.

These mistakes **disable the mod even if `pcall` catches the error**; from the mistake on,
both world functions refuse every call, and none of the mod's writes of that call is applied:
- calling a function whose capability `mod.json` does not declare;
- a block name the game does not know (`set_block`);
- more than 64 queued writes in one call (writes to chunks that are not loaded do not count).

## Capabilities

A mod declares what it needs in `mod.json`, for example
`"capabilities": ["world.read", "world.write"]`. The game prints each running mod's
capabilities at startup.

| Capability | Allows |
|---|---|
| `world.read` | `world:get_block` |
| `world.write` | `world:set_block` |

Both concern the game world only, so declaring them is enough. Capabilities that reach the
player's machine (files, network) will need the player's explicit grant; none exists in v1.

## What is not there, and why

| Missing | Why |
|---|---|
| `io`, `os`, `package`, `require`, `dofile`, `loadfile`, `load` | No file, process or code-loading access: a mod reaches the game only through `vox`. |
| `debug` | It can read and change any value, including the host's. |
| `coroutine` | Not needed by event handlers yet; fewer places where the budget has to follow. |
| `collectgarbage` | A full collection can take long and the budget cannot interrupt it. |
| `xpcall` | Its message handler runs where the host cannot stop a spent budget; use `pcall`. |
| `warn` | It writes outside the mod's bounded log; use `print`. |
| `string.dump` | It makes bytecode, the classic way out of a Lua sandbox. |
| `string.match`, `string.gmatch`, `string.gsub`, `string.pack`, `string.unpack`, `string.packsize` | Patterns backtrack in C, where no budget reaches. They return later through a budgeted matcher, as a minor version. |
| `__gc` in `setmetatable` | A finalizer runs whenever the collector decides, also between calls, where no budget applies. `setmetatable` with a `__gc` field is an error. |

`string.find(s, text, init)` always searches `text` as **plain text** (a fourth argument is
accepted and ignored). Kept from `string`: `byte`, `char`, `find`, `format`, `len`, `lower`,
`rep`, `reverse`, `sub`, `upper`. `table`, `math` and `utf8` are complete, but the `table`
functions take only plain tables: a table whose metatable has `__index`, `__newindex` or
`__len` is an error there (the C code would call scripts or trust a script's length).

## Limits

A mod that breaks a limit is **disabled for the session**: its script stops, its state is
dropped, and the game prints why. Its blocks and textures stay. Catching the error with `pcall`
does not help against an exhausted instruction or time budget: it stays fatal, and from then on
no code of the mod runs, not even `__close` methods. A memory error is fatal too when `pcall`
sees it; a script can hide it from `pcall` (for example with a `__close` method that raises
another error), and then keeps running, still held at its memory cap.

| Limit | Value (provisional) |
|---|---|
| Lua instructions per event | 200,000 |
| Block writes per event | 64 |
| Lua instructions at startup | 10,000,000 |
| Time per event | 50 ms |
| Time at startup | 2 s |
| Memory per mod | 4 MiB |
| Nested `pcall`, `print` and library calls | 16 levels |
| Error message shown | 4096 bytes, one line |

Library functions that work in C (`string`, `table`, `utf8` and the `utf8.codes` iterator,
`tonumber`) and the world functions count toward the instruction limit: one instruction per
argument and per result, four more per world function call for the lookup, one per 32 bytes
they read or write, one per element the table functions visit, at least one
per repetition of `string.rep`, and for `table.sort` one per byte of every string element per
comparison. A loop over a long string therefore runs out like any other loop.

## Known gaps (residual risks, measured in step S7)

These are not charged by their real cost. They cannot reach outside the mod, and the time limit
stops a loop of them, but only at its next check, so one call can run longer than the limit
before the mod is disabled:
- `..`, `<`, `<=` and `==` on two long strings are one instruction each, however long the
  strings are. The worst measured case, `<` on two 1.9 MB strings of `\0` bytes in a loop,
  stalled one call for about 1.3 s (release build, development machine).
- `next` and `pairs` over a table with many emptied slots, `tostring` of a value whose
  metatable has a long `__name`, and copying many values through `...`, `select`, calls or
  host functions.

All values are provisional and are fixed by measurement on a low-end device (step S7).
