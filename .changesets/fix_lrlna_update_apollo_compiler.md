### Use less recursion in GraphQL parser ([Issue #4142](https://github.com/apollographql/router/issues/4142))

This updates the Router with the latest `apollo-parser` version,
which removes unnecessary use of recursion for parsing repeated syntax elements
such as enum values and union members in type definitions.
Some documents that used to hit the parser’s recursion limit will now successfully parse.

By [@lrlna](https://github.com/lrlna) in https://github.com/apollographql/router/pull/4167
