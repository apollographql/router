wit_bindgen::generate!({
    path: "../../../apollo-router/wit/router-plugin",
    world: "router-plugin",
});

use exports::apollo::router_plugin::hooks::BreakResponse;
use exports::apollo::router_plugin::hooks::ContextEntry;
use exports::apollo::router_plugin::hooks::ContextOperation;
use exports::apollo::router_plugin::hooks::Event;
use exports::apollo::router_plugin::hooks::Guest;
use exports::apollo::router_plugin::hooks::Header;
use exports::apollo::router_plugin::hooks::HeaderOperation;
use exports::apollo::router_plugin::hooks::Mutation;
use exports::apollo::router_plugin::hooks::Outcome;

struct RustHeader;

impl Guest for RustHeader {
    fn handle(event: Event) -> Result<Outcome, String> {
        if event.configuration.contains("break-request") {
            return Ok(Outcome::BreakRequest(BreakResponse {
                status_code: 403,
                headers: vec![Header {
                    name: "x-wasm-rust".to_string(),
                    values: vec!["blocked".to_string()],
                }],
                body: r#"{"errors":[{"message":"blocked by Rust WASM plugin"}]}"#
                    .to_string(),
            }));
        }

        Ok(Outcome::Proceed(Mutation {
            headers: vec![HeaderOperation::Set(Header {
                name: "x-wasm-rust".to_string(),
                values: vec!["active".to_string()],
            })],
            context: vec![ContextOperation::Set(ContextEntry {
                name: "wasm.rust".to_string(),
                value: r#"{"language":"rust"}"#.to_string(),
            })],
            body: None,
        }))
    }
}

export!(RustHeader);
