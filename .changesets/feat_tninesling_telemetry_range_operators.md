### Add gt and lt operators for telemetry conditions ([PR #5048](https://github.com/apollographql/router/pull/5048))

Adds greater than and less than operators for telemetry conditions called `gt` and `lt`, respectively. The configuration for both takes two arguments as a list, similar to `eq`. The `gt` operator checks that the first argument is greater than the second, and, similarly, the `lt` operator checks that the first argument is less than the second.. Other conditions such as `gte`, `lte`, and `range` can all be made from combinations of `gt`, `lt`, `eq`, and `all`.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/5048
