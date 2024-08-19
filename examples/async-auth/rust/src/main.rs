//! ```text
//! curl -v \
//!     --header 'content-type: application/json' \
//!     --header 'x-client-id: unknown' \
//!     --url 'http://127.0.0.1:4000' \
//!     --data '{"query":"query Query {\n  me {\n    name\n  }\n}"}'
//! [...]
//! < HTTP/1.1 403 Forbidden
//! < content-length: 78
//! < date: Mon, 07 Mar 2022 12:08:21 GMT
//! <
//! * Connection #0 to host 127.0.0.1 left intact
//!
//! {"errors":[{"message":"client-id is not allowed","locations":[],"path":null}]}
//! curl -v \
//!     --header 'content-type: application/json' \
//!     --header 'x-client-id: jeremy' \
//!     --url 'http://127.0.0.1:4000' \
//!     --data '{"query":"query Query {\n  me {\n    name\n  }\n}"}'
//! < HTTP/1.1 200 OK
//! < content-length: 39
//! < date: Mon, 07 Mar 2022 12:09:08 GMT
//! <
//! * Connection #0 to host 127.0.0.1 left intact
//!
//! {"data":{"me":{"name":"Ada Lovelace"}}}
//! ```

// adding the module to your main.rs file
// will automatically register it to the router plugin registry.
//
// you can use the plugin by adding it to `config.yml`
mod allow_client_id_from_file;

use anyhow::Result;

// `cargo run -- -s ../../graphql/supergraph.graphql -c ./router.yaml`
fn main() -> Result<()> {
    apollo_router::main()
}
