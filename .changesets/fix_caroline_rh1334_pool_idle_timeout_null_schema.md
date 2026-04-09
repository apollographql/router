### Fix schema validation rejecting `pool_idle_timeout: null`

Setting `pool_idle_timeout: null` in `traffic_shaping` configuration caused a startup failure with a schema validation error, despite `null` being a documented valid value that disables idle connection eviction. The JSON schema incorrectly only allowed strings; it now correctly accepts both strings and `null`.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/XXXX
