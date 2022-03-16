//! curl -v \
//!     --header 'content-type: application/json' \
//!     --cookie 'yummy_cookie=choco' \
//!     --cookie 'tasty_cookie=strawberry' \
//!     --url 'http://127.0.0.1:4000' \
//!     --data '{"query":"query Query {\n  me {\n    name\n  }\n}"}'
//! [...]
//! XXX DON'T FORGET TO UPDATE THE RESPONSE
//! < HTTP/1.1 403 Forbidden
//! < content-length: 242
//! < date: Thu, 10 Mar 2022 13:47:30 GMT
//! <
//! * Connection #0 to host 127.0.0.1 left intact
//! {"errors":[{"message":"eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpYXQiOjE2NDY4MTM1NzMsImV4cCI6MTY0NjgyMDc3MywibmJmIjoxNjQ2ODEzNTczfQ.vywNkhZ7mX2KU8cu6o1FG4xNYR7YvXyavzzta9g7fQE is not authorized: Token has expired","locations":[],"path":null}]}

use anyhow::Result;

// `cargo run -- -s ../graphql/supergraph.graphql -c ./config.yml`
fn main() -> Result<()> {
    apollo_router::main()
}
