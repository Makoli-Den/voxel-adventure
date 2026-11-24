print(string.format("hello from %s, api %s", "hello", vox.api_version))
vox.on("block_broken", function()
  print("hello saw a block break")
end)
