### Coprocessors: Discard content-length sent by coprocessors. ([PR #3802](https://github.com/apollographql/router/pull/3802))

The `content-length` of an HTTP response can only be computed when a router response is being sent.
We now discard coprocessors `content-length` header to make sure the value is computed correctly.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/3802
