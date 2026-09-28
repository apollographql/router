### Connector validation no longer rejects some `->` method calls that work at runtime

Composition could reject connector expressions that succeed at runtime, such as `$args.ids->map(@)->joinNotNull(",")`, because the shape checks for several `->` methods were stricter than the methods themselves. Validation now only rejects a method call when no possible value could make it succeed. Thanks to [@fernando-apollo](https://github.com/fernando-apollo) for reporting the `->joinNotNull` case.

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/10286
