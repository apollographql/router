### Optimize Query::apply_root_selection_set map lookups ([PR #9458](https://github.com/apollographql/router/pull/9458))

Combines three separate map operations in `Query::apply_root_selection_set`, resulting in a 5-15% performance improvement.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9458
