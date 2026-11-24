-- Tries to survive the instruction budget by catching its error.
vox.on("block_placed", function()
  pcall(function() while true do end end)
  print("survived")
end)
