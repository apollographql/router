### Router starts on arm64 Linux kernels with 16K or 64K pages ([Issue #3382](https://github.com/apollographql/router/issues/3382))

The arm64 Linux binary and Docker image exited at startup with `<jemalloc>: Unsupported system page size` on kernels that use 16K or 64K memory pages, such as the Raspberry Pi 5's default kernel. jemalloc is now built for 64K pages on `aarch64-unknown-linux-gnu`, so the same binary runs on 4K, 16K and 64K kernels. x86_64 and macOS builds are unchanged.

By [@cujarrett](https://github.com/cujarrett) in https://github.com/apollographql/router/pull/10595
