### Fix metrics attribute types ([Issue #3687](https://github.com/apollographql/router/issues/3687))

Metrics attributes were being coerced to strings. This is now fixed.
In addition, the logic around types accepted as metrics attributes has been simplified. It will log and ignore values of the wrong type.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/3724
