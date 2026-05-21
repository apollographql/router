### Restore `cargo-llvm-cov` to mise so nightly macOS coverage stops failing

The `cargo:cargo-llvm-cov` pin was dropped from `.config/mise/config.toml` during the May 2025 mise migration. The `do_coverage` CircleCI step still invokes `cargo llvm-cov nextest`, with no installer anywhere else in the repo. Nightly `coverage-macos_test` has been red on `dev` since 2026-04-09 (~30 consecutive runs) once the macOS executor stopped providing the binary out of band. Re-pin the latest stable (`0.8.7`).

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9497
