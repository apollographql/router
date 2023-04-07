### Coprocessors: Empty body requests from `GET` requests are now deserialized without error 

Fixes a bug where a coprocessor operating at the `router_request` stage would fail to deserialize an empty body, which is typical for `GET` requests.

By [@o0ignition0o](https://github.com/o0ignition0o) in https://github.com/apollographql/router/pull/2863
