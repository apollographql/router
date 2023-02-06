### CI: Enable compliance checks /except/ licenses.html update ([Issue #2514](https://github.com/apollographql/router/issues/2514))

In [#1573](https://github.com/apollographql/router/pull/1573), we removed the compliance checks for non-release CI pipelines, because the cargo-about output would change ever so slightly.
Some checks however are very important and prevent us from inadvertently downgrading libraries and needing to open [#2512](https://github.com/apollographql/router/pull/2512).

This changeset includes the following:
- Introduce `cargo xtask licenses` to update licenses.html.
- Separate compliance (cargo-deny, which includes license checks) and licenses generation (cargo-about) in xtask
- Enable compliance as part of our CI checks for each open PR
- Update `cargo xtask all` so it runs tests, checks compliance and updates licenses.html
- Introduce `cargo xtask dev` so it checks compliance and runs tests

Use `cargo xtask all`  to make sure everything is up to date before a release.
Use `cargo xtask dev` before a PR.

Updating licenses.html is now driven by `cargo xtask licenses`, which is part of the release checklist.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/2520
