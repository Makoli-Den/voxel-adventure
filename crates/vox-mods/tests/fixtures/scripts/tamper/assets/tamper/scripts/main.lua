-- Everything here changes only this mod's own state.
string.format = function() return "tampered" end
getmetatable("").__index.rep = nil
setmetatable(_G, { __newindex = function() error("frozen") end })
print(string.format("%s", "ignored"))
