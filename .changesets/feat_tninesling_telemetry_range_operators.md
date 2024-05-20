### Add gt and lt operators for telemetry conditions ([PR #5048](https://github.com/apollographql/router/pull/5048))

The router supports greater than (`gt`) and less than (`lt`) operators for telemetry conditions. Similar to the `eq` operator, the configuration for both `gt` and `lt` takes two arguments as a list. The `gt` operator checks that the first argument is greater than the second, and the `lt` operator checks that the first argument is less than the second. Other conditions such as `gte`, `lte`, and `range` can be made from combinations of `gt`, `lt`, `eq`, and `all`.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/5048
