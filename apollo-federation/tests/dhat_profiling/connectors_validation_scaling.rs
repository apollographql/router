use apollo_federation::connectors::validation::Code;
use apollo_federation::connectors::validation::Message;

#[global_allocator]
pub(crate) static ALLOC: dhat::Alloc = dhat::Alloc;

// Failure of the test can be diagnosed using the dhat-heap.json file.

// Two synthetic subgraphs on which connectors validation used to be super-linear. Each is
// profiled around `connectors::validation::validate` alone, after parsing and link expansion, so
// the peak is validation's own. As in `connectors_validation.rs`, each limit is ~10% above the
// measured peak; the regressions they guard against are orders of magnitude larger.
//
// One test running both cases in turn, rather than two tests: a dhat profiler is process-wide,
// and tests in one binary run on parallel threads.
#[test]
fn connectors_validation_scales_linearly() {
    // A literal array of 800 objects. The selection walk copied every field seen so far at each
    // object it visited, so memory grew quadratically: 168 MB before the fix, 4.4 MB after.
    let items = vec![r#"{ a: "x", b: "y", c: "z" }"#; 800].join(", ");
    let literal_array = format!(
        r#"{HEADER}
        type Item {{ a: String b: String c: String }}
        type Query {{
          items: [Item] @connect(source: "api", http: {{ GET: "/n" }}, selection: """$([{items}]) {{ a b c }}""")
        }}
        "#
    );
    let messages = profile_validation("literal_array_800", &literal_array, 4_800_000);
    assert!(messages.is_empty(), "{messages:?}");

    // 16 levels of types, each referring to the next through two fields, like `billingAddress`
    // and `shippingAddress: Address` repeated down a hierarchy, plus a field connector on
    // the root type so `$this` is the whole graph. Inlining each referenced type made every
    // shape 2^16 copies of the bottom type: 1.9 GB before the fix, 322 KB after.
    let mut dag = String::from(HEADER);
    for level in 0..16 {
        let refs = if level < 15 {
            format!("a: D{next} b: D{next}", next = level + 1)
        } else {
            String::new()
        };
        let connector = if level == 0 {
            r#"extra: String @connect(source: "api", http: { GET: "/x/{$this.id}" }, selection: "$")"#
        } else {
            ""
        };
        dag.push_str(&format!("type D{level} {{ id: ID {refs} {connector} }}\n"));
    }
    dag.push_str(
        r#"type Query { d: D0 @connect(source: "api", http: { GET: "/n" }, selection: "id") }"#,
    );
    let messages = profile_validation("dag_16", &dag, 360_000);
    // Only `D0.id` and `D0.extra` are resolved by a connector, so every other field is reported
    // as unresolved. That check runs last, so seeing nothing but it means the whole validation
    // ran.
    assert!(!messages.is_empty());
    assert!(
        messages
            .iter()
            .all(|message| message.code == Code::ConnectorsUnresolvedField),
        "{messages:?}"
    );
}

const HEADER: &str = r#"
extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.11")
  @link(url: "https://specs.apollo.dev/connect/v0.4", import: ["@connect", "@source"])
  @source(name: "api", http: { baseURL: "https://api.example.com" })
"#;

fn profile_validation(name: &str, sdl: &str, max_bytes: usize) -> Vec<Message> {
    let subgraph =
        apollo_federation::subgraph::typestate::Subgraph::parse(name, "http://test", sdl)
            .unwrap()
            .expand_links()
            .unwrap();

    let _profiler = dhat::Profiler::builder().testing().build();
    let messages = apollo_federation::connectors::validation::validate(&subgraph);
    let stats = dhat::HeapStats::get();

    dhat::assert!(
        stats.max_bytes < max_bytes,
        "{name}: {} > {max_bytes}",
        stats.max_bytes,
    );
    messages
}
