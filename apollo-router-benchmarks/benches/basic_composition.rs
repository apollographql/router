use apollo_router_core::{
    plugin_utils, PluggableRouterServiceBuilder, Request, Response, ResponseBody, RouterRequest,
    RouterResponse, Schema, SubgraphRequest, SubgraphResponse, Value,
};
use criterion::{criterion_group, criterion_main, Criterion};
use futures::future;
use once_cell::sync::Lazy;
use serde_json_bytes::ByteString;
use std::{collections::HashMap, sync::Arc, task::Poll};
use tower::{util::BoxCloneService, BoxError, Service, ServiceExt};

type MockResponses = HashMap<Request, Response>;

#[derive(Default, Clone)]
struct MockSubgraph {
    // using an arc here because benchmarks will clone a lot of services
    mocks: Arc<MockResponses>,
}

impl MockSubgraph {
    pub fn new(mocks: MockResponses) -> Self {
        Self {
            mocks: Arc::new(mocks),
        }
    }
}

impl Service<SubgraphRequest> for MockSubgraph {
    type Response = SubgraphResponse;

    type Error = BoxError;

    type Future = future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: SubgraphRequest) -> Self::Future {
        let builder = plugin_utils::SubgraphResponse::builder().context(req.context);
        let response = if let Some(response) = self.mocks.get(req.http_request.body()) {
            builder.data(response.data.clone()).build().into()
        } else {
            builder
                .errors(vec![apollo_router_core::Error {
                    message: "couldn't find mock for query".to_string(),
                    locations: Default::default(),
                    path: Default::default(),
                    extensions: Default::default(),
                }])
                .build()
                .into()
        };
        future::ok(response)
    }
}

static EXPECTED_RESPONSE: Lazy<ResponseBody> = Lazy::new(|| {
    ResponseBody::GraphQL(serde_json::from_str(r#"{"data":{"topProducts":[{"upc":"1","name":"Table","reviews":[{"id":"1","product":{"name":"Table"},"author":{"id":"1","name":"Ada Lovelace"}},{"id":"4","product":{"name":"Table"},"author":{"id":"2","name":"Alan Turing"}}]},{"upc":"2","name":"Couch","reviews":[{"id":"2","product":{"name":"Couch"},"author":{"id":"1","name":"Ada Lovelace"}}]}]}}"#).unwrap())
});

static QUERY: &str = r#"query TopProducts($first: Int) { topProducts(first: $first) { upc name reviews { id product { name } author { id name } } } }"#;

async fn basic_composition_benchmark(
    mut router_service: BoxCloneService<RouterRequest, RouterResponse, BoxError>,
) {
    let request = plugin_utils::RouterRequest::builder()
        .query(QUERY.to_string())
        .variables(Arc::new(
            vec![(ByteString::from("first"), Value::Number(2usize.into()))]
                .into_iter()
                .collect(),
        ))
        .build()
        .into();

    let response = router_service
        .ready()
        .await
        .unwrap()
        .call(request)
        .await
        .unwrap();
    assert_eq!(response.response.body(), &*EXPECTED_RESPONSE,);
}

fn from_elem(c: &mut Criterion) {
    let account_mocks = vec![
        (
            r#"{"query":"query($representations:[_Any!]!){_entities(representations:$representations){...on User{name}}}","variables":{"representations":[{"__typename":"User","id":"1"},{"__typename":"User","id":"2"},{"__typename":"User","id":"1"}]}}"#,
            r#"{"data":{"_entities":[{"name":"Ada Lovelace"},{"name":"Alan Turing"},{"name":"Ada Lovelace"}]}}"#
        )
    ].into_iter().map(|(query, response)| (serde_json::from_str(query).unwrap(), serde_json::from_str(response).unwrap())).collect();
    let account_service = MockSubgraph::new(account_mocks);

    let review_mocks = vec![
        (
            r#"{"query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Product{reviews{id product{__typename upc}author{__typename id}}}}}","variables":{"representations":[{"__typename":"Product","upc":"1"},{"__typename":"Product","upc":"2"}]}}"#,
            r#"{"data":{"_entities":[{"reviews":[{"id":"1","product":{"__typename":"Product","upc":"1"},"author":{"__typename":"User","id":"1"}},{"id":"4","product":{"__typename":"Product","upc":"1"},"author":{"__typename":"User","id":"2"}}]},{"reviews":[{"id":"2","product":{"__typename":"Product","upc":"2"},"author":{"__typename":"User","id":"1"}}]}]}}"#
        ),
        ].into_iter().map(|(query, response)| (serde_json::from_str(query).unwrap(), serde_json::from_str(response).unwrap())).collect();
    let review_service = MockSubgraph::new(review_mocks);

    let product_mocks = vec![
        (
            r#"{"query":"query($first:Int){topProducts(first:$first){__typename upc name}}","variables":{"first":2}}"#,
            r#"{"data":{"topProducts":[{"__typename":"Product","upc":"1","name":"Table"},{"__typename":"Product","upc":"2","name":"Couch"}]}}"#
        ),
        (
            r#"{"query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Product{name}}}","variables":{"representations":[{"__typename":"Product","upc":"1"},{"__typename":"Product","upc":"1"},{"__typename":"Product","upc":"2"}]}}"#,
            r#"{"data":{"_entities":[{"name":"Table"},{"name":"Table"},{"name":"Couch"}]}}"#
        )
        ].into_iter().map(|(query, response)| (serde_json::from_str(query).unwrap(), serde_json::from_str(response).unwrap())).collect();
    let product_service = MockSubgraph::new(product_mocks);

    let schema: Arc<Schema> =
        Arc::new(include_str!("fixtures/supergraph.graphql").parse().unwrap());

    c.bench_function("basic_composition_benchmark", move |b| {
        let runtime = tokio::runtime::Runtime::new().unwrap();

        let builder = PluggableRouterServiceBuilder::new(schema.clone());

        let builder = builder
            .with_subgraph_service("accounts", account_service.clone())
            .with_subgraph_service("reviews", review_service.clone())
            .with_subgraph_service("products", product_service.clone());

        let (router, _) = runtime.block_on(builder.build());
        b.to_async(runtime)
            .iter(|| basic_composition_benchmark(router.clone()));
    });
}

criterion_group!(benches, from_elem);
criterion_main!(benches);
