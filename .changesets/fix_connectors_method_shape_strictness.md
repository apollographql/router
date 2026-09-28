### Connector validation no longer rejects some `->` method calls that work at runtime

Composition could reject connector expressions that succeed at runtime, such as `$args.ids->map(@)->joinNotNull(",")`, because the shape checks for several `->` methods were stricter than the methods themselves. Validation now only rejects a method call when no possible value could make it succeed.

The same applies to `isSuccess`, which counts a missing or non-boolean value as failure at runtime. Expressions like `$args.s?->eq("a")` or a `->match` without a boolean fallback no longer fail composition. Only an `isSuccess` expression that can never be a boolean, like `$.items->size`, is rejected.

Thanks to [@fernando-apollo](https://github.com/fernando-apollo) for reporting the `->joinNotNull` case.

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/10286
