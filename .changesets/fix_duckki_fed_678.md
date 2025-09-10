### (Federation) Removed `RebaseError::InterfaceObjectTypename` variant ([PR #8109](https://github.com/apollographql/router/pull/8109))

Fixed an uncommon query planning error, "Cannot add selection of field `X` to selection set of parent type `Y` that is potentially an interface object type at runtime". Although fetching `__typename` selections from interface object types are unnecessary, it is difficult to avoid them in all cases and the effect of having those selections in query plans is benign. Thus, the error variant and the check for the error have been removed.

By [@duckki](https://github.com/duckki) in https://github.com/apollographql/router/pull/8109
