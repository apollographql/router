### An empty `plugins` key is rejected

Router v2.x treated `plugins: null` the same as `plugins: {}`. YAML reads a `plugins:` key with nothing under it as `null`, for example when every plugin beneath it is commented out. The router now rejects that configuration with `null is not of type "object"` at the `plugins` key, as it does any other value of the wrong type. The configuration JSON Schema no longer allows `null` for `plugins`, and its default is now `{}`.

To migrate, delete the empty `plugins:` key or write `plugins: {}`.

By [@BrynCooke](https://github.com/BrynCooke)
