### raise the default Redis timeout to 500ms ([PR #5795](https://github.com/apollographql/router/pull/5795))

The default Redis command timeout was initially set at 2ms, which is too low for most production usage. It is now raised to 500ms by default.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5795