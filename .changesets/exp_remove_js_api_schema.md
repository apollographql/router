### Enable Rust-based API schema implementation ([PR #5623](https://github.com/apollographql/router/pull/5623))

We have been testing the Rust-based API schema generation implementation side by side with the old JavaScript-based implementation for a few months now, and observed no difference in behaviour. Now, the Router only uses the Rust-based implementation, which is faster and more robust.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/5623