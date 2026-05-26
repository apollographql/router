### Improve `Query::apply_root_selection_set` performance by 5-15% ([PR #9458](https://github.com/apollographql/router/pull/9458))

`Query::apply_root_selection_set` now combines three separate map lookups into one, reducing work on every query plan application by 5-15%.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9458
