### Ban openssl in cargo-deny ([PR #2510](https://github.com/apollographql/router/pull/2638))

This change introduces a ban of openssl-sys in the project, with exceptions on redis and redis-cluster-async.

This will allow us to prevent us from mistakenly reintroducing it in the future.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/2638
