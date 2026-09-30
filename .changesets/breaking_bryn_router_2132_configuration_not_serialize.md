### `apollo_router::Configuration` no longer implements `Serialize`

The router never serialized its `Configuration`. The implementation existed only for tests, wrote secrets out in full, and failed for a configuration assembled in code. Code that serialized a `Configuration`, for example with `serde_json::to_value` or `serde_yaml::to_string`, no longer compiles. Serialize the configuration document you parse the `Configuration` from instead.

By [@BrynCooke](https://github.com/BrynCooke)
