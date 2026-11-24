for i = 1, 1000 do
  print("line " .. i)
end
vox.on("block_broken", function()
  for i = 1, 1000 do print(i) end
end)
