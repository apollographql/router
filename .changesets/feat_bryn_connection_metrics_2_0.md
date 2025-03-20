### Add `apollo.router.open_connections` metric ([PR #7023](https://github.com/apollographql/router/pull/7023))

To help users to diagnose when connections are keeping pipelines hanging around the following metric has been added:
- `apollo.router.open_connections` - The number of request pipelines active in the router
    - `schema.id` - The Apollo Studio schema hash associated with the pipeline.
    - `launch.id` - The Apollo Studio launch id associated with the pipeline (optional).
    - `config.hash` - The hash of the configuration.
    - `server.address` - The address that the router is listening on.
    - `server.port` - The port that the router is listening on if not a unix socket.
    - `state` - Either `active` or `terminating`.

Connections can be held open by clients via keepalive or even just a long running request, so it's useful to know when this is happening.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/7023