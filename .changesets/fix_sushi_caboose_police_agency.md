### Remove leading 0s before parsing into `serde_json::Number` ([PR #6766](https://github.com/apollographql/router/pull/6766))

We previously decided to allow leading 0s, per [this discussion](https://github.com/apollographql/router/pull/5762#discussion_r1711807550).

However, since we use the parsing logic of `serde_json::Number` to produce the internal representation, and that logic fails given leading zeros, we need to normalize the input `&str` a bit before calling `number.parse()`. We already perform some similar normalizations, e.g. ensuring the fractional part is at least a 0 (not empty after the `.`), so there's precedent.

Thanks to @nicholascioli for finding this problem using fuzz testing!

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/6766