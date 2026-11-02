### Connector validation reports errors in `->` method arguments and accepts `null` array elements

Composition now rejects expressions with an error inside most `->` method arguments, like `$(true)->and($(1)->gt("x"))`, which always fail at runtime. `->in`, `->contains` and `->joinNotNull` no longer reject array elements that are `null` or have no value, which the runtime skips.

Thanks to [@fernando-apollo](https://github.com/fernando-apollo), [@briannafugate408](https://github.com/briannafugate408) and [@TylerBloom](https://github.com/TylerBloom) for the reviews.

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/10316
