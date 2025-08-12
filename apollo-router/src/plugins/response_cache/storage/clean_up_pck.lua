local pck_key = KEYS[1]
local cache_tags_key = KEYS[2]

if redis.call('EXISTS', pck_key) == 1 then
    return ""
else
    return redis.call('GETDEL', cache_tags_key)
end

