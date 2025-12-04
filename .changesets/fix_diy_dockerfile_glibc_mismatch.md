### Prevent glibc mismatch in DIY Docker images ([Issue #8450](https://github.com/apollographql/router/issues/8450))

The DIY Dockerfile now pins the Rust builder to the Bookworm variant (for example, `rust:1.91.1-slim-bookworm`) so the builder and runtime share the same Debian base. This prevents the image from failing at startup with `/lib/x86_64-linux-gnu/libc.so.6: version 'GLIBC_2.39' not found`.

This resolves a regression introduced when the `rust:1.90.0` bump used a generic Rust image without specifying a Debian variant. The upstream Rust image default advanced to a newer variant with glibc 2.39 while the DIY runtime remained on Bookworm, creating a version mismatch.

By [@theJC](https://github.com/theJC) in https://github.com/apollographql/router/pull/8629
