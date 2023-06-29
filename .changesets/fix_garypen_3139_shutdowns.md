### Preserve all shutdown receivers across reloads ([Issue #3139](https://github.com/apollographql/router/issues/3139))

Preserve a vec of all channels that we create and process all of them during shutdown. This will avoid scenarios such as:

- some requests are in flight
- the router reloads (new schema, etc)
- the router gets a shutdown signal
- since the shutdown channel for the older configuration is not kept, the router closes immediately without waiting for the initial connections to stop

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3311