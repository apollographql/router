local key = KEYS[1]

local members = redis.call('ZRANGE', key, 0, -1)
redis.call('DEL', key)

return members
