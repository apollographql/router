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
