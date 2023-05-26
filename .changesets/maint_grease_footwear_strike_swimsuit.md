### chore(deps): `xtask/` dependency updates ([PR #3149](https://github.com/apollographql/router/pull/3149))

This is effectively running `cargo update` in the `xtask/` directory (our
directory of tooling; not runtime components) to bring things more up to
date.

This changeset takes extra care to update `chrono`'s features to remove the
`time` dependency which is impacted by CVE-2020-26235.

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/3149