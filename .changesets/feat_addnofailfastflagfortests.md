### Use CI use --no-fail-fast when running tests ([Issue #8435](https://github.com/apollographql/router/issues/8435))

- When running cargo xtask test, have the ability to run all tests, not just until the first one fails.

- Have CI builds use this, so that after all the time and effort ($$$) spent building the binaries and spinning up containers, the full suite of tests are run so reports contain the results for all tests, and not just the first (often flaky) test. If there are any real issues, a developer will then see the whole picture/impact of their change.

By [Jon Christiansen](https://github.com/theJC) in https://github.com/apollographql/router/pull/8431
