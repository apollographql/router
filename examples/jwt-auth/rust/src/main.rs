//! curl -v \
//!     --header 'content-type: application/json' \
//!     --header 'authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpYXQiOjE2NDY4MTM1NzMsImV4cCI6MTY0NjgyMDc3MywibmJmIjoxNjQ2ODEzNTczfQ.vywNkhZ7mX2KU8cu6o1FG4xNYR7YvXyavzzta9g7fQE' \
//!     --url 'http://127.0.0.1:4000' \
//!     --data '{"query":"query Query {\n  me {\n    name\n  }\n}"}'
//! [...]
//! < HTTP/1.1 403 Forbidden
//! < content-length: 242
//! < date: Thu, 10 Mar 2022 13:47:30 GMT
//! <
//! * Connection #0 to host 127.0.0.1 left intact
//! {"errors":[{"message":"eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpYXQiOjE2NDY4MTM1NzMsImV4cCI6MTY0NjgyMDc3MywibmJmIjoxNjQ2ODEzNTczfQ.vywNkhZ7mX2KU8cu6o1FG4xNYR7YvXyavzzta9g7fQE is not authorized: Token has expired","locations":[],"path":null}]}

use anyhow::Result;

// adding the module to your main.rs file
// will automatically register it to the router plugin registry.
//
// you can use the plugin by adding it to `router.yaml`
mod jwt;

// `cargo run -- -s ../../graphql/supergraph.graphql -c ./router.yaml`
fn main() -> Result<()> {
    apollo_router::main()
}
