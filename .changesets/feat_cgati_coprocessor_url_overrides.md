### Support per-stage coprocessor URLs ([PR #8384](https://github.com/apollographql/router/pull/8384))

You can now configure different coprocessor URLs for each stage of request/response processing (router, supergraph, execution, subgraph). Each stage can specify its own `url` field that overrides the global default URL.

Changes:
- Add optional `url` field to all stage configuration structs
- Update all stage `as_service` methods to accept and resolve URLs
- Add tests for URL validation and per-stage configuration

This change maintains full backward compatibility—existing configurations with a single global URL continue to work unchanged.

By [@cgati](https://github.com/cgati) in https://github.com/apollographql/router/pull/8384
