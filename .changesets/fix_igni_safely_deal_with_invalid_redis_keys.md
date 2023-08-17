### Redis storage: return an error instead if a non serializable value is sent. ([#3594](https://github.com/apollographql/router/issues/3594))

This changeset returns an error if a value couldn't be serialized before being sent to the redis storage backend.
It also logs the error in console and prompts you to open an issue (This message showing up would be a router bug!).

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/3597
