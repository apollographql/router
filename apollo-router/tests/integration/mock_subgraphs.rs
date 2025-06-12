use apollo_router::_private::mock_subgraphs_subgraph_call;
use apollo_router::graphql::Request;
use serde_json_bytes::json;

#[test]
fn test_surrogate_keys() {
    let sdl = include_str!("../fixtures/supergraph.graphql");
    let supergraph = apollo_federation::Supergraph::new(sdl).unwrap();
    let subgraphs = supergraph.extract_subgraphs().unwrap();

    let schema = subgraphs.get("products").unwrap().schema.schema();
    let config = json!({
        "query": {
            "topProducts": [
                {"upc": "1", "__surrogateKeys": ["topProducts"]},
                {"upc": "2"},
            ],
        },
    });
    let query = "{ topProducts { upc } }";
    let request = Request::fake_builder().query(query).build();
    let response = mock_subgraphs_subgraph_call(config.clone(), schema, &request).unwrap();
    insta::assert_yaml_snapshot!(response, @r###"
    data:
      topProducts:
        - upc: "1"
        - upc: "2"
    extensions:
      apolloSurrogateKeys:
        - topProducts
    "###);

    let schema = subgraphs.get("reviews").unwrap().schema.schema();
    let config = json!({
        "entities": [
            {
                "__surrogateKeys": ["product-1"],
                "__typename": "Product",
                "upc": "1",
                "reviews": [{"id": "r1a"}, {"id": "r1b"}],
            },
            {
                "__surrogateKeys": ["product-2"],
                "__typename": "Product",
                "upc": "2",
                "reviews": [{"id": "r2"}],
            },
        ],
    });
    let query = r#"
        {
            _entities(representations: [{upc: "2"}, {upc: "1"}]) {
                ... on Product {
                    reviews { id }
                }
            }
        }
    "#;
    let request = Request::fake_builder().query(query).build();
    let response = mock_subgraphs_subgraph_call(config.clone(), schema, &request).unwrap();
    insta::assert_yaml_snapshot!(response, @r###"
    data:
      _entities:
        - reviews:
            - id: r2
        - reviews:
            - id: r1a
            - id: r1b
    extensions:
      apolloEntitySurrogateKeys:
        - - product-2
        - - product-1
    "###);
}
