local hoard = {}
for i = 1, 1e9 do
  hoard[i] = string.rep("x", 1024) .. i
end
