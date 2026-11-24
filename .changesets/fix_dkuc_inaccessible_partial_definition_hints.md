### Skip partial-definition composition hints for `@inaccessible` output elements ([PR #10888](https://github.com/apollographql/router/pull/10888))

Composition no longer emits `INCONSISTENT_OBJECT_VALUE_TYPE_FIELD`, `INCONSISTENT_INTERFACE_VALUE_TYPE_FIELD`, `INCONSISTENT_ENUM_VALUE_FOR_OUTPUT_ENUM`, or `INCONSISTENT_UNION_MEMBER` when the element that only some subgraphs define is marked `@inaccessible`, either directly or through its parent type. This matches the existing behavior for `@inaccessible` input object fields and input enum values, and lets you stage a new field, enum value, or union member with `@inaccessible` without composition hints failing your checks.

By [@dariuszkuc](https://github.com/dariuszkuc) in https://github.com/apollographql/router/pull/10888
