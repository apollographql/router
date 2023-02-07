### Run rustfmt on xtask too ([Issue #2557](https://github.com/apollographql/router/issues/2557))

`xtask` which runs `cargo fmt --all` which reformats of Rust code in all crates of the workspace. However the code of xtask itself is a separate workspace. In order for it to be formatted with the same configuration, running a second `cargo` command is required. This adds that second command, and applies the corresponding formatting.

Fixes https://github.com/apollographql/router/issues/2557

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/2561
