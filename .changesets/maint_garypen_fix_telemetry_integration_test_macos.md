### Fix integration test warning on macOS ([PR #4919](https://github.com/apollographql/router/pull/4919))

Previously, integration tests of the router on macOS could produce the warning messages:

```
warning: unused import: `common::Telemetry`
 --> apollo-router/tests/integration/mod.rs:4:16
  |
4 | pub(crate) use common::Telemetry;
  |                ^^^^^^^^^^^^^^^^^
  |
  = note: `#[warn(unused_imports)]` on by default

warning: unused import: `common::ValueExt`
 --> apollo-router/tests/integration/mod.rs:5:16
  |
5 | pub(crate) use common::ValueExt;
  |                ^^^^^^^^^^^^^^^^
```

That issue is now resolved. 

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/4919
