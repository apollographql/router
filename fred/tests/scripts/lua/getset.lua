#!lua name=getsetlib

local function getset(keys, args)
  local old = redis.call('GET', keys[1])
  local new = redis.call('SET', keys[1], args[1])
  return old
end

redis.register_function('getset', getset)