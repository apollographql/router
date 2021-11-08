# this doc explains the wasm setup for now, until we find a good way to write that into the docs

## Setup

Rust focused for now.

get Rust: https://rustup.rs/

```
rustup target add wasm32-wasi
```

## example project

```
cargo new wasm-example && cd wasm-example
```

Cargo.toml:

```toml
[package]
name = "wasm-example"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"
crate-type = ["cdylib"]

[dependencies]
wee_alloc = "0.4.5"
```

src/lib.rs:
```
use std::{str, slice};

#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

#[no_mangle]
pub extern fn hello(ptr: *const u8, len: u64) -> u64 {
    let s = unsafe {
        let slice = slice::from_raw_parts(ptr, len as usize);
        str::from_utf8(slice).unwrap()
    };

    let s2 = format!("hello, {}", s);

    0
}
```

Build:

```
cargo build --target wasm32-wasi
```

the wasm file is in `target/wasm32-wasi/debug/wasm_example.wasm`

## TODO

investigate WITX for the host API
https://bytecodealliance.org/articles/implementing-wasi-nn-in-wasmtime

investigate externref to pass data from host to guest (like HTTP headers) to avoid copies


investigate the envoy wasm ecosystem:
https://github.com/proxy-wasm/proxy-wasm-rust-sdk/blob/master/examples/http_auth_random.rs
https://github.com/proxy-wasm/proxy-wasm-cpp-host/blob/master/src/wasmtime/wasmtime.cc
https://www.solo.io/blog/the-state-of-webassembly-in-envoy-proxy/
https://docs.solo.io/web-assembly-hub/latest/tutorial_code/getting_started/
ABI spec: https://github.com/proxy-wasm/spec/tree/master/abi-versions/vNEXT
https://github.com/proxy-wasm/proxy-wasm-rust-sdk/blob/abd0f5437212e5fd3dd6a70eac3959934278e643/src/traits.rs#L438
