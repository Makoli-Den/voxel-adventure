-- Example mod: placed stone grows into a small pillar, and breaking a stone block takes
-- down the stone stacked on it. It has no data of its own; it uses the base game's blocks.
-- Run it with: cargo run -p vox-game -- --mods mods --mods examples/mods

-- How many blocks a pillar grows above the placed stone.
local PILLAR_HEIGHT = 4
local STONE = "base:stone"
local AIR = "core:air"

vox.on("block_placed", function(event, world)
  if event.block ~= STONE then
    return
  end
  local grown = 0
  for dy = 1, PILLAR_HEIGHT do
    local y = event.y + dy
    -- `nil` above the loaded terrain layer: the pillar stops there.
    if world:get_block(event.x, y, event.z) ~= AIR then
      break
    end
    world:set_block(event.x, y, event.z, STONE)
    grown = grown + 1
  end
  print(string.format("a pillar of %d grew on the stone at %d %d %d", grown, event.x, event.y, event.z))
end)

vox.on("block_broken", function(event, world)
  if event.block ~= STONE then
    return
  end
  local removed = 0
  for dy = 1, PILLAR_HEIGHT do
    local y = event.y + dy
    if world:get_block(event.x, y, event.z) ~= STONE then
      break
    end
    world:set_block(event.x, y, event.z, AIR)
    removed = removed + 1
  end
  print(string.format("stone broken at %d %d %d, %d stone above it fell", event.x, event.y, event.z, removed))
end)
