### Enable Rust-based API schema implementation ([PR #5623](https://github.com/apollographql/router/pull/5623))

The router has transitioned to solely using a Rust-based API schema generation implementation. 

Previously, the router used a Javascript-based implementation. After testing for a few months, we've validated the improved performance and robustness of the new Rust-based implementation, so the router now only uses it.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/5623