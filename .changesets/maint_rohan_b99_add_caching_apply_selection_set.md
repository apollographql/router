### Add caching to `Query::apply_selection_set` ([PR #9592](https://github.com/apollographql/router/pull/9592))

Adds fragment caching to `Query::apply_selection_set` to significantly reduce time spent formatting responses from operations with deeply nested fragments where the fragment is not spread on the root.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9592
