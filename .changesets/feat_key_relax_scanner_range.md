### Coprocessor: add fields to the router and the subgraph stage ([Issue #2861](https://github.com/apollographql/router/issues/2861), [Issue #2861](https://github.com/apollographql/router/issues/2862))

This changeset adds several (read only) fields to coprocessor stages:

router request:
    - uri
    - method

router response:
    - status_code

subgraph response:
    - status_code

It also fixes a bug where a coprocessor operating at the `router_request` stage would fail to deserialize an empty body (which happens for GET requests).

By [@o0ignition0o](https://github.com/o0ignition0o) in https://github.com/apollographql/router/pull/2863
