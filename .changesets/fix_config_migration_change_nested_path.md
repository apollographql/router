### Config migrations can now rewrite values on deeply nested paths

The `change` action in the router's configuration migration machinery rewrites a config value only when it currently equals an expected value. It performed that comparison with a JSONPath filter expression that silently matches nothing once the path is more than two segments deep, so any migration targeting a deeply nested path did nothing at all — with no error and no warning, making it look like the migration had run successfully.

The comparison is now done by selecting the value at the path and comparing it directly, which behaves the same at any depth. The `change` action is also now documented alongside the other migration actions.

No shipped migration was affected, since the only existing use of `change` targets a two-segment path.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9917
