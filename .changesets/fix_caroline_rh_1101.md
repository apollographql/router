### Support more types of nullable elements in response/entity cache keys ([PR #8923](https://github.com/apollographql/router/pull/8923))

[PR #8767](https://github.com/apollographql/router/pull/8767) (released in Router v2.11.0) changed the entity and response caching keys to support nullable elements. The implementation covered the case of a field explicitly being set to null, but didn't cover the following cases:

- Nullable field being missing
- Nullable list items

This change adds support for those cases.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/8923
