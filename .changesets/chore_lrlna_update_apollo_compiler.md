### Updates apollo-compiler with a fix to parser's recursion([PR #4167](https://github.com/apollographql/router/pull/4167))

This updates the Router with the newest release of `apollo-compiler`. The
release includes a fix to parser's internal recursive functions which caused an
inaccurate `recursion_limit` counts.

By [@lrlna](https://github.com/lrlna) in https://github.com/apollographql/router/pull/4167