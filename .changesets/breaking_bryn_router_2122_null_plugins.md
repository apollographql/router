### An empty `plugins` key is an error

Router v2.x ignored a `plugins:` key with nothing under it, which you get when every plugin beneath it is commented out. The router now refuses to load that configuration and reports `null is not of type "object"` at the `plugins` key.

To migrate, delete the empty `plugins:` key or write `plugins: {}`.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10602
