### Fred doesn't seem to return an is_not_found error if the error does not exist ([Issue #2876](https://github.com/apollographql/router/issues/2876))

The Redis Fred client doesn't seem to deal with `nil` responses correctly returning a parse error instead of not found.
This means that if there was a cache miss for a key, the Router would log a parse error.

We now manually detect `nil` responses and treat them as a cache miss.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/3661
