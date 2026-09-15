### Add fragment caching to `Query::apply_root_selection_set` ([PR #9469](https://github.com/apollographql/router/pull/9469))

Adds fragment caching to `Query::apply_root_selection_set` to significantly reduce time spent formatting responses from operations with deeply nested fragments.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9469
