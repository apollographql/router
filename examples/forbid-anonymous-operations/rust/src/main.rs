//! ```text
//! curl -v \
//!     --header 'content-type: application/json' \
//!     --url 'http://127.0.0.1:4000' \
//!     --data '{"query":"query Query {\n  me {\n    name\n  }\n}"}'
//! [...]
//! < HTTP/1.1 400 Bad Request
//! < content-length: 90
//! < date: Thu, 03 Mar 2022 14:31:50 GMT
//! <
//! * Connection #0 to host 127.0.0.1 left intact
//! {"errors":[{"message":"Anonymous operations are not allowed","locations":[],"path":null}]}
//! ```

use anyhow::Result;

// adding the module to your main.rs file
// will automatically register it to the router plugin registry.
//
// you can use the plugin by adding it to `router.yaml`
mod forbid_anonymous_operations;

// `cargo run -- -s ../../graphql/supergraph.graphql -c ./router.yaml`
fn main() -> Result<()> {
    apollo_router::main()
}
