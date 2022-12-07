//! Implements the router phase of the request lifecycle.

use std::sync::Arc;
use std::task::Poll;

use axum::body::StreamBody;
use axum::response::*;
use bytes::Buf;
use bytes::Bytes;
use futures::future::ready;
use futures::future::BoxFuture;
use futures::stream;
use futures::stream::once;
use futures::stream::StreamExt;
use http::header::CONTENT_TYPE;
use http::header::VARY;
use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use http::StatusCode;
use http_body::Body as _;
use hyper::Body;
use multimap::MultiMap;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;

use super::layers::static_page::StaticPageLayer;
use super::new_service::ServiceFactory;
use super::router;
use super::supergraph;
use super::StuffThatHasPlugins;
#[cfg(test)]
use super::SupergraphCreator;
use super::MULTIPART_DEFER_CONTENT_TYPE;
use crate::graphql;
#[cfg(test)]
use crate::plugin::test::MockSupergraphService;
use crate::plugins::content_type::APPLICATION_JSON_HEADER_VALUE;
use crate::plugins::content_type::GRAPHQL_JSON_RESPONSE_HEADER_VALUE;
use crate::router_factory::RouterFactory;
use crate::Configuration;
use crate::Endpoint;
use crate::ListenAddr;
use crate::RouterRequest;
use crate::RouterResponse;
use crate::SupergraphRequest;
use crate::SupergraphResponse;

/// Containing [`Service`] in the request lifecyle.
#[derive(Clone)]
pub(crate) struct RouterService<SF>
where
    SF: ServiceFactory<supergraph::Request> + Clone + Send + Sync + 'static,
{
    supergraph_creator: Arc<SF>,
}

impl<SF> RouterService<SF>
where
    SF: ServiceFactory<supergraph::Request> + Clone + Send + Sync + 'static,
{
    pub(crate) fn new(supergraph_creator: Arc<SF>) -> Self {
        RouterService { supergraph_creator }
    }
}

#[cfg(test)]
pub(crate) async fn from_supergraph_mock_callback_and_configuration(
    supergraph_callback: impl FnMut(supergraph::Request) -> supergraph::ServiceResult
        + Send
        + Sync
        + 'static
        + Clone,
    configuration: Arc<Configuration>,
) -> impl Service<
    router::Request,
    Response = router::Response,
    Error = BoxError,
    Future = BoxFuture<'static, router::ServiceResult>,
> + Send {
    let mut supergraph_service = MockSupergraphService::new();

    supergraph_service.expect_clone().returning(move || {
        let cloned_callback = supergraph_callback.clone();
        let mut supergraph_service = MockSupergraphService::new();
        supergraph_service.expect_call().returning(cloned_callback);
        supergraph_service
    });

    RouterCreator::new(
        Arc::new(SupergraphCreator::for_tests(supergraph_service).await),
        &configuration,
    )
    .make()
}

#[cfg(test)]
pub(crate) async fn from_supergraph_mock_callback(
    supergraph_callback: impl FnMut(supergraph::Request) -> supergraph::ServiceResult
        + Send
        + Sync
        + 'static
        + Clone,
) -> impl Service<
    router::Request,
    Response = router::Response,
    Error = BoxError,
    Future = BoxFuture<'static, router::ServiceResult>,
> + Send {
    from_supergraph_mock_callback_and_configuration(
        supergraph_callback,
        Arc::new(Configuration::default()),
    )
    .await
}

#[cfg(test)]
pub(crate) async fn empty() -> impl Service<
    router::Request,
    Response = router::Response,
    Error = BoxError,
    Future = BoxFuture<'static, router::ServiceResult>,
> + Send {
    let mut supergraph_service = MockSupergraphService::new();
    supergraph_service
        .expect_clone()
        .returning(MockSupergraphService::new);

    RouterCreator::new(
        Arc::new(SupergraphCreator::for_tests(supergraph_service).await),
        &Configuration::default(),
    )
    .make()
}

impl<SF> Service<RouterRequest> for RouterService<SF>
where
    SF: ServiceFactory<supergraph::Request> + Clone + Send + Sync + 'static,
    <SF as ServiceFactory<supergraph::Request>>::Service:
        Service<supergraph::Request, Response = supergraph::Response, Error = BoxError> + Send,
    <<SF as ServiceFactory<supergraph::Request>>::Service as Service<supergraph::Request>>::Future:
        Send,
{
    type Response = RouterResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: RouterRequest) -> Self::Future {
        let RouterRequest {
            router_request,
            context,
        } = req;

        let (parts, body) = router_request.into_parts();

        let supergraph_service = self.supergraph_creator.create();
        let fut = async move {
            let graphql_request = if parts.method == Method::GET {
                parts
                    .uri
                    .query()
                    .and_then(|q| graphql::Request::from_urlencoded_query(q.to_string()).ok())
            } else {
                hyper::body::to_bytes(body)
                    .await
                    .ok()
                    .and_then(|bytes| serde_json::from_reader(bytes.reader()).ok())
                    .flatten()
            };

            if let Some(graphql_request) = graphql_request {
                let request = SupergraphRequest {
                    supergraph_request: http::Request::from_parts(parts, graphql_request),
                    context,
                };

                let SupergraphResponse { response, context } =
                    supergraph_service.oneshot(request).await.unwrap();

                let accepts_wildcard: bool = context
                    .get("accepts-wildcard")
                    .unwrap_or_default()
                    .unwrap_or_default();
                let accepts_json: bool = context
                    .get("accepts-json")
                    .unwrap_or_default()
                    .unwrap_or_default();
                let accepts_multipart: bool = context
                    .get("accepts-multipart")
                    .unwrap_or_default()
                    .unwrap_or_default();

                let (mut parts, mut body) = response.into_parts();
                process_vary_header(&mut parts.headers);

                match body.next().await {
                    None => {
                        tracing::error!("router service is not available to process request",);
                        Ok(router::Response {
                            response: http::Response::builder()
                                .status(StatusCode::SERVICE_UNAVAILABLE)
                                .body(Body::from(
                                    "router service is not available to process request",
                                ))
                                .expect("cannot fail"),
                            context,
                        })
                    }
                    Some(response) => {
                        if !response.has_next.unwrap_or(false) && (accepts_json || accepts_wildcard)
                        {
                            parts
                                .headers
                                .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
                            tracing::trace_span!("serialize_response").in_scope(|| {
                                // TODO: writer?
                                let body = serde_json::to_string(&response).unwrap();
                                Ok(router::Response {
                                    response: http::Response::from_parts(parts, Body::from(body)),
                                    context,
                                })
                            })
                        } else if accepts_multipart {
                            parts.headers.insert(
                                CONTENT_TYPE,
                                HeaderValue::from_static(MULTIPART_DEFER_CONTENT_TYPE),
                            );

                            // each chunk contains a response and the next delimiter, to let client parsers
                            // know that they can process the response right away
                            let mut first_buf = Vec::from(
                                &b"\r\n--graphql\r\ncontent-type: application/json\r\n\r\n"[..],
                            );
                            serde_json::to_writer(&mut first_buf, &response).unwrap();
                            if response.has_next.unwrap_or(false) {
                                first_buf.extend_from_slice(b"\r\n--graphql\r\n");
                            } else {
                                first_buf.extend_from_slice(b"\r\n--graphql--\r\n");
                            }

                            let body =
                                once(ready(Ok(Bytes::from(first_buf)))).chain(body.map(|res| {
                                    let mut buf =
                                        Vec::from(&b"content-type: application/json\r\n\r\n"[..]);
                                    serde_json::to_writer(&mut buf, &res).unwrap();

                                    // the last chunk has a different end delimiter
                                    if res.has_next.unwrap_or(false) {
                                        buf.extend_from_slice(b"\r\n--graphql\r\n");
                                    } else {
                                        buf.extend_from_slice(b"\r\n--graphql--\r\n");
                                    }

                                    Ok::<_, BoxError>(buf.into())
                                }));

                            let response =
                                (parts, StreamBody::new(body)).into_response().map(|body| {
                                    // Axum makes this `body` have type:
                                    // https://docs.rs/http-body/0.4.5/http_body/combinators/struct.UnsyncBoxBody.html
                                    let mut body = Box::pin(body);
                                    // We make a stream based on its `poll_data` method
                                    // in order to create a `hyper::Body`.
                                    Body::wrap_stream(stream::poll_fn(move |ctx| {
                                        body.as_mut().poll_data(ctx)
                                    }))
                                    // … but we ignore the `poll_trailers` method:
                                    // https://docs.rs/http-body/0.4.5/http_body/trait.Body.html#tymethod.poll_trailers
                                    // Apparently HTTP/2 trailers are like headers, except after the response body.
                                    // I (Simon) believe nothing in the Apollo Router uses trailers as of this writing,
                                    // so ignoring `poll_trailers` is fine.
                                    // If we want to use trailers, we may need remove this convertion to `hyper::Body`
                                    // and return `UnsyncBoxBody` (a.k.a. `axum::BoxBody`) as-is.
                                });

                            Ok(RouterResponse { response, context })
                        } else {
                            // this should be unreachable due to a previous check, but just to be sure...
                            Ok(router::Response {
                                response: http::Response::builder()
                                    .status(StatusCode::NOT_ACCEPTABLE)
                                    .body(Body::from(
                                        format!(
                                            r#"'accept' header can't be different from \"*/*\", {:?}, {:?} or {:?}"#,
                                            APPLICATION_JSON_HEADER_VALUE,
                                            GRAPHQL_JSON_RESPONSE_HEADER_VALUE,
                                            MULTIPART_DEFER_CONTENT_TYPE
                                        )
                                    ))
                                    .expect("cannot fail"),
                                context,
                            })
                        }
                    }
                }
            } else {
                // BAD REQUEST
                Ok(router::Response {
                    response: http::Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(Body::from("Invalid GraphQL request"))
                        .expect("cannot fail"),
                    context,
                })
            }
        };
        Box::pin(fut)
    }
}

// Process the headers to make sure that `VARY` is set correctly
fn process_vary_header(headers: &mut HeaderMap<HeaderValue>) {
    if headers.get(VARY).is_none() {
        // We don't have a VARY header, add one with value "origin"
        headers.insert(VARY, HeaderValue::from_static("origin"));
    }
}

/// A collection of services and data which may be used to create a "router".
#[derive(Clone)]
pub(crate) struct RouterCreator<SF>
where
    SF: ServiceFactory<supergraph::Request> + Clone + Send + Sync + 'static,
{
    supergraph_creator: Arc<SF>,
    static_page: StaticPageLayer,
}

impl<SF> ServiceFactory<router::Request> for RouterCreator<SF>
where
    SF: StuffThatHasPlugins + ServiceFactory<supergraph::Request> + Clone + Send + Sync + 'static,
    <SF as ServiceFactory<supergraph::Request>>::Service:
        Service<supergraph::Request, Response = supergraph::Response, Error = BoxError> + Send,
    <<SF as ServiceFactory<supergraph::Request>>::Service as Service<supergraph::Request>>::Future:
        Send,
{
    type Service = router::BoxService;
    fn create(&self) -> Self::Service {
        self.make().boxed()
    }
}

impl<SF> RouterFactory for RouterCreator<SF>
where
    SF: StuffThatHasPlugins + ServiceFactory<supergraph::Request> + Clone + Send + Sync + 'static,
    <SF as ServiceFactory<supergraph::Request>>::Service:
        Service<supergraph::Request, Response = supergraph::Response, Error = BoxError> + Send,
    <<SF as ServiceFactory<supergraph::Request>>::Service as Service<supergraph::Request>>::Future:
        Send,
{
    type RouterService = router::BoxService;

    type Future = <<RouterCreator<SF> as ServiceFactory<router::Request>>::Service as Service<
        router::Request,
    >>::Future;

    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
        let mut mm = MultiMap::new();
        self.supergraph_creator
            .plugins()
            .values()
            .for_each(|p| mm.extend(p.web_endpoints()));
        mm
    }
}

impl<SF> RouterCreator<SF>
where
    SF: StuffThatHasPlugins + ServiceFactory<supergraph::Request> + Clone + Send + Sync + 'static,
    <SF as ServiceFactory<supergraph::Request>>::Service:
        Service<supergraph::Request, Response = supergraph::Response, Error = BoxError> + Send,
    <<SF as ServiceFactory<supergraph::Request>>::Service as Service<supergraph::Request>>::Future:
        Send,
{
    pub(crate) fn new(supergraph_creator: Arc<SF>, configuration: &Configuration) -> Self {
        let static_page = StaticPageLayer::new(configuration);
        Self {
            supergraph_creator,
            static_page,
        }
    }

    pub(crate) fn make(
        &self,
    ) -> impl Service<
        router::Request,
        Response = router::Response,
        Error = BoxError,
        Future = BoxFuture<'static, router::ServiceResult>,
    > + Send {
        let router_service = RouterService::new(self.supergraph_creator.clone());

        ServiceBuilder::new()
            .layer(self.static_page.clone())
            .service(
                self.supergraph_creator
                    .plugins()
                    .iter()
                    .rev()
                    .fold(router_service.boxed(), |acc, (_, e)| e.router_service(acc)),
            )
    }
}

#[cfg(test)]
mod tests {
    use http::Uri;
    use serde_json_bytes::json;

    use super::*;
    use crate::plugin::test::MockSubgraph;
    use crate::plugins::content_type::APPLICATION_JSON_HEADER_VALUE;
    use crate::services::supergraph;
    use crate::test_harness::MockedSubgraphs;
    use crate::Context;
    use crate::TestHarness;

    // Test Vary processing

    #[test]
    fn it_adds_default_with_value_origin_if_no_vary_header() {
        let mut default_headers = HeaderMap::new();
        process_vary_header(&mut default_headers);
        let vary_opt = default_headers.get(VARY);
        assert!(vary_opt.is_some());
        let vary = vary_opt.expect("has a value");
        assert_eq!(vary, "origin");
    }

    #[test]
    fn it_leaves_vary_alone_if_set() {
        let mut default_headers = HeaderMap::new();
        default_headers.insert(VARY, HeaderValue::from_static("*"));
        process_vary_header(&mut default_headers);
        let vary_opt = default_headers.get(VARY);
        assert!(vary_opt.is_some());
        let vary = vary_opt.expect("has a value");
        assert_eq!(vary, "*");
    }

    #[test]
    fn it_leaves_varys_alone_if_there_are_more_than_one() {
        let mut default_headers = HeaderMap::new();
        default_headers.insert(VARY, HeaderValue::from_static("one"));
        default_headers.append(VARY, HeaderValue::from_static("two"));
        process_vary_header(&mut default_headers);
        let vary = default_headers.get_all(VARY);
        assert_eq!(vary.iter().count(), 2);
        for value in vary {
            assert!(value == "one" || value == "two");
        }
    }

    #[tokio::test]
    async fn it_extracts_query_and_operation_name() {
        let query = "query";
        let expected_query = query;
        let operation_name = "operationName";
        let expected_operation_name = operation_name;

        let expected_response = graphql::Response::builder()
            .data(json!({"response": "yay"}))
            .build();

        let mut router_service = super::from_supergraph_mock_callback(move |req| {
            let example_response = expected_response.clone();

            assert_eq!(
                req.supergraph_request.body().query.as_deref().unwrap(),
                expected_query
            );
            assert_eq!(
                req.supergraph_request
                    .body()
                    .operation_name
                    .as_deref()
                    .unwrap(),
                expected_operation_name
            );

            Ok(SupergraphResponse::new_from_graphql_response(
                example_response,
                req.context,
            ))
        })
        .await;

        // get request
        let get_path = format!(
            "/?{}",
            serde_urlencoded::to_string(&[("query", query), ("operationName", operation_name)])
                .unwrap(),
        );

        let get_uri = Uri::builder().path_and_query(get_path).build().unwrap();

        let get_request = http::Request::builder()
            .method(Method::GET)
            .uri(get_uri)
            .body(hyper::Body::empty())
            .unwrap();

        router_service.call(get_request.into()).await.unwrap();

        // post request
        let post_request = supergraph::Request::builder()
            .query(query)
            .operation_name(operation_name)
            .header(CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE)
            .uri(Uri::from_static("/"))
            .method(Method::POST)
            .context(Context::new())
            .build()
            .unwrap();

        router_service
            .call(post_request.try_into().unwrap())
            .await
            .unwrap();
    }

    const SCHEMA: &str = r#"schema
        @core(feature: "https://specs.apollo.dev/core/v0.1")
        @core(feature: "https://specs.apollo.dev/join/v0.1")
        @core(feature: "https://specs.apollo.dev/inaccessible/v0.1")
         {
        query: Query
   }
   directive @core(feature: String!) repeatable on SCHEMA
   directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet) on FIELD_DEFINITION
   directive @join__type(graph: join__Graph!, key: join__FieldSet) repeatable on OBJECT | INTERFACE
   directive @join__owner(graph: join__Graph!) on OBJECT | INTERFACE
   directive @join__graph(name: String!, url: String!) on ENUM_VALUE
   directive @inaccessible on OBJECT | FIELD_DEFINITION | INTERFACE | UNION
   scalar join__FieldSet

   enum join__Graph {
       USER @join__graph(name: "user", url: "http://localhost:4001/graphql")
       ORGA @join__graph(name: "orga", url: "http://localhost:4002/graphql")
   }

   type Query {
       currentUser: User @join__field(graph: USER)
   }

   type User
   @join__owner(graph: USER)
   @join__type(graph: ORGA, key: "id")
   @join__type(graph: USER, key: "id"){
       id: ID!
       name: String
       activeOrganization: Organization
   }

   type Organization
   @join__owner(graph: ORGA)
   @join__type(graph: ORGA, key: "id")
   @join__type(graph: USER, key: "id") {
       id: ID
       creatorUser: User
       name: String
       nonNullId: ID!
       suborga: [Organization]
   }"#;

    #[tokio::test]
    async fn nullability_formatting() {
        let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{currentUser{activeOrganization{__typename id}}}"}},
                serde_json::json!{{"data": {"currentUser": { "activeOrganization": null }}}}
            ).build()),
        ("orga", MockSubgraph::default())
    ].into_iter().collect());

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(SCHEMA)
            .extra_plugin(subgraphs)
            .build_router()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .query("query { currentUser { activeOrganization { id creatorUser { name } } } }")
            // Request building here
            .build()
            .unwrap();
        let response = service
            .oneshot(request.try_into().unwrap())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap()
            .unwrap();

        let json_response: graphql::Response =
            serde_json::from_slice(response.to_vec().as_slice()).unwrap();

        insta::assert_json_snapshot!(json_response);
    }

    #[tokio::test]
    async fn nullability_bubbling() {
        let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{currentUser{activeOrganization{__typename id}}}"}},
                serde_json::json!{{"data": {"currentUser": { "activeOrganization": {} }}}}
            ).build()),
        ("orga", MockSubgraph::default())
    ].into_iter().collect());

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(SCHEMA)
            .extra_plugin(subgraphs)
            .build_router()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .query(
                "query { currentUser { activeOrganization { nonNullId creatorUser { name } } } }",
            )
            .build()
            .unwrap();
        let response = service
            .oneshot(request.try_into().unwrap())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap()
            .unwrap();

        let json_response: graphql::Response =
            serde_json::from_slice(response.to_vec().as_slice()).unwrap();

        insta::assert_json_snapshot!(json_response);
    }

    #[tokio::test]
    async fn errors_on_deferred_responses() {
        let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{currentUser{__typename id}}"}},
                serde_json::json!{{"data": {"currentUser": { "__typename": "User", "id": "0" }}}}
            )
            .with_json(
                serde_json::json!{{
                    "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on User{name}}}",
                    "variables": {
                        "representations":[{"__typename": "User", "id":"0"}]
                    }
                }},
                serde_json::json!{{
                    "data": {
                        "_entities": [{ "suborga": [
                        { "__typename": "User", "name": "AAA"},
                        ] }]
                    },
                    "errors": [
                        {
                            "message": "error user 0",
                            "path": ["_entities", 0],
                        }
                    ]
                    }}
            ).build()),
        ("orga", MockSubgraph::default())
    ].into_iter().collect());

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(SCHEMA)
            .extra_plugin(subgraphs)
            .build_router()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .header("Accept", "multipart/mixed; deferSpec=20220824")
            .query("query { currentUser { id  ...@defer { name } } }")
            .build()
            .unwrap();
        let mut stream = service.oneshot(request.try_into().unwrap()).await.unwrap();

        insta::assert_json_snapshot!(std::str::from_utf8(
            stream
                .next_response()
                .await
                .unwrap()
                .unwrap()
                .to_vec()
                .as_slice()
        )
        .unwrap());

        insta::assert_json_snapshot!(std::str::from_utf8(
            stream
                .next_response()
                .await
                .unwrap()
                .unwrap()
                .to_vec()
                .as_slice()
        )
        .unwrap());
    }

    #[tokio::test]
    async fn deferred_fragment_bounds_nullability() {
        let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{currentUser{activeOrganization{__typename id}}}"}},
                serde_json::json!{{"data": {"currentUser": { "activeOrganization": { "__typename": "Organization", "id": "0" } }}}}
            ).build()),
        ("orga", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{suborga{__typename id}}}}",
                "variables": {
                    "representations":[{"__typename": "Organization", "id":"0"}]
                }
            }},
            serde_json::json!{{
                "data": {
                    "_entities": [{ "suborga": [
                    { "__typename": "Organization", "id": "1"},
                    { "__typename": "Organization", "id": "2"},
                    { "__typename": "Organization", "id": "3"},
                    ] }]
                },
                }}
        )
        .with_json(
            serde_json::json!{{
                "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{name}}}",
                "variables": {
                    "representations":[
                        {"__typename": "Organization", "id":"1"},
                        {"__typename": "Organization", "id":"2"},
                        {"__typename": "Organization", "id":"3"}

                        ]
                }
            }},
            serde_json::json!{{
                "data": {
                    "_entities": [
                    { "__typename": "Organization", "id": "1"},
                    { "__typename": "Organization", "id": "2", "name": "A"},
                    { "__typename": "Organization", "id": "3"},
                    ]
                },
                "errors": [
                    {
                        "message": "error orga 1",
                        "path": ["_entities", 0],
                    },
                    {
                        "message": "error orga 3",
                        "path": ["_entities", 2],
                    }
                ]
                }}
        ).build())
    ].into_iter().collect());

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(SCHEMA)
            .extra_plugin(subgraphs)
            .build_router()
            .await
            .unwrap();

        let supergraph_request = supergraph::Request::fake_builder()
            .header("Accept", "multipart/mixed; deferSpec=20220824")
            .query(
                "query { currentUser { activeOrganization { id  suborga { id ...@defer { nonNullId } } } } }",
            )
            .build()
            .unwrap();

        let router_request = supergraph_request.try_into().unwrap();

        let mut stream = service.oneshot(router_request).await.unwrap();

        insta::assert_json_snapshot!(std::str::from_utf8(
            stream
                .next_response()
                .await
                .unwrap()
                .unwrap()
                .to_vec()
                .as_slice()
        )
        .unwrap());

        insta::assert_json_snapshot!(std::str::from_utf8(
            stream
                .next_response()
                .await
                .unwrap()
                .unwrap()
                .to_vec()
                .as_slice()
        )
        .unwrap());
    }

    #[tokio::test]
    async fn errors_on_incremental_responses() {
        let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder().with_json(
                serde_json::json!{{"query":"{currentUser{activeOrganization{__typename id}}}"}},
                serde_json::json!{{"data": {"currentUser": { "activeOrganization": { "__typename": "Organization", "id": "0" } }}}}
            ).build()),
        ("orga", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{suborga{__typename id}}}}",
                "variables": {
                    "representations":[{"__typename": "Organization", "id":"0"}]
                }
            }},
            serde_json::json!{{
                "data": {
                    "_entities": [{ "suborga": [
                    { "__typename": "Organization", "id": "1"},
                    { "__typename": "Organization", "id": "2"},
                    { "__typename": "Organization", "id": "3"},
                    ] }]
                },
                }}
        )
        .with_json(
            serde_json::json!{{
                "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{name}}}",
                "variables": {
                    "representations":[
                        {"__typename": "Organization", "id":"1"},
                        {"__typename": "Organization", "id":"2"},
                        {"__typename": "Organization", "id":"3"}

                        ]
                }
            }},
            serde_json::json!{{
                "data": {
                    "_entities": [
                    { "__typename": "Organization", "id": "1"},
                    { "__typename": "Organization", "id": "2", "name": "A"},
                    { "__typename": "Organization", "id": "3"},
                    ]
                },
                "errors": [
                    {
                        "message": "error orga 1",
                        "path": ["_entities", 0],
                    },
                    {
                        "message": "error orga 3",
                        "path": ["_entities", 2],
                    }
                ]
                }}
        ).build())
    ].into_iter().collect());

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(SCHEMA)
            .extra_plugin(subgraphs)
            .build_router()
            .await
            .unwrap();

        let supergraph_request = supergraph::Request::fake_builder()
            .header("Accept", "multipart/mixed; deferSpec=20220824")
            .query(
                "query { currentUser { activeOrganization { id  suborga { id ...@defer { name } } } } }",
            ).build()
            .unwrap();

        let router_request = supergraph_request.try_into().unwrap();

        let mut stream = service.oneshot(router_request).await.unwrap();
        insta::assert_json_snapshot!(std::str::from_utf8(
            stream
                .next_response()
                .await
                .unwrap()
                .unwrap()
                .to_vec()
                .as_slice()
        )
        .unwrap());
        insta::assert_json_snapshot!(std::str::from_utf8(
            stream
                .next_response()
                .await
                .unwrap()
                .unwrap()
                .to_vec()
                .as_slice()
        )
        .unwrap());
    }

    #[tokio::test]
    async fn query_reconstruction() {
        let schema = r#"schema
    @link(url: "https://specs.apollo.dev/link/v1.0")
    @link(url: "https://specs.apollo.dev/join/v0.2", for: EXECUTION)
    @link(url: "https://specs.apollo.dev/tag/v0.2")
    @link(url: "https://specs.apollo.dev/inaccessible/v0.2", for: SECURITY)
  {
    query: Query
    mutation: Mutation
  }
  
  directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION
  
  directive @join__field(graph: join__Graph!, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
  
  directive @join__graph(name: String!, url: String!) on ENUM_VALUE
  
  directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
  
  directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
  
  directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
  
  directive @tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION
  
  scalar join__FieldSet
  
  enum join__Graph {
    PRODUCTS @join__graph(name: "products", url: "http://products:4000/graphql")
    USERS @join__graph(name: "users", url: "http://users:4000/graphql")
  }
  
  scalar link__Import
  
  enum link__Purpose {
    SECURITY
    EXECUTION
  }
  
  type MakePaymentResult
    @join__type(graph: USERS)
  {
    id: ID!
    paymentStatus: PaymentStatus
  }
  
  type Mutation
    @join__type(graph: USERS)
  {
    makePayment(userId: ID!): MakePaymentResult!
  }
  
  
 type PaymentStatus
    @join__type(graph: USERS)
  {
    id: ID!
  }
  
  type Query
    @join__type(graph: PRODUCTS)
    @join__type(graph: USERS)
  {
    name: String
  }
  "#;

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(schema)
            .build_router()
            .await
            .unwrap();

        let supergraph_request = supergraph::Request::fake_builder()
            .header("Accept", "multipart/mixed; deferSpec=20220824")
            .query(
                r#"mutation ($userId: ID!) {
                    makePayment(userId: $userId) {
                      id
                      ... @defer {
                        paymentStatus {
                          id
                        }
                      }
                    }
                  }"#,
            )
            .build()
            .unwrap();

        let router_request = supergraph_request.try_into().unwrap();

        let mut stream = service.oneshot(router_request).await.unwrap();

        insta::assert_json_snapshot!(std::str::from_utf8(
            stream
                .next_response()
                .await
                .unwrap()
                .unwrap()
                .to_vec()
                .as_slice()
        )
        .unwrap());
    }

    // if a deferred response falls under a path that was nullified in the primary response,
    // the deferred response must not be sent
    #[tokio::test]
    async fn filter_nullified_deferred_responses() {
        let subgraphs = MockedSubgraphs([
        ("user", MockSubgraph::builder()
        .with_json(
            serde_json::json!{{"query":"{currentUser{__typename name id}}"}},
            serde_json::json!{{"data": {"currentUser": { "__typename": "User", "name": "Ada", "id": "1" }}}}
        )
        .with_json(
            serde_json::json!{{
                "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on User{activeOrganization{__typename id}}}}",
                "variables": {
                    "representations":[{"__typename": "User", "id":"1"}]
                }
            }},
            serde_json::json!{{
                "data": {
                    "_entities": [
                        {
                            "activeOrganization": {
                                "__typename": "Organization", "id": "2"
                            }
                        }
                    ]
                }
                }})
                .with_json(
                    serde_json::json!{{
                        "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on User{name}}}",
                        "variables": {
                            "representations":[{"__typename": "User", "id":"3"}]
                        }
                    }},
                    serde_json::json!{{
                        "data": {
                            "_entities": [
                                {
                                    "name": "A"
                                }
                            ]
                        }
                        }})
       .build()),
        ("orga", MockSubgraph::builder()
        .with_json(
            serde_json::json!{{
                "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{creatorUser{__typename id}}}}",
                "variables": {
                    "representations":[{"__typename": "Organization", "id":"2"}]
                }
            }},
            serde_json::json!{{
                "data": {
                    "_entities": [
                        {
                            "creatorUser": {
                                "__typename": "User", "id": "3"
                            }
                        }
                    ]
                }
                }})
                .with_json(
                    serde_json::json!{{
                        "query":"query($representations:[_Any!]!){_entities(representations:$representations){...on Organization{nonNullId}}}",
                        "variables": {
                            "representations":[{"__typename": "Organization", "id":"2"}]
                        }
                    }},
                    serde_json::json!{{
                        "data": {
                            "_entities": [
                                {
                                    "nonNullId": null
                                }
                            ]
                        }
                        }}).build())
    ].into_iter().collect());

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(SCHEMA)
            .extra_plugin(subgraphs)
            .build_router()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .query(
                r#"query {
                currentUser {
                    name
                    ... @defer {
                        activeOrganization {
                            id
                            nonNullId
                            ... @defer {
                                creatorUser {
                                    name
                                }
                            }
                        }
                    }
                }
            }"#,
            )
            .header("Accept", "multipart/mixed; deferSpec=20220824")
            .build()
            .unwrap();
        let mut response = service.oneshot(request.try_into().unwrap()).await.unwrap();

        let primary = response.next_response().await.unwrap().unwrap();
        insta::assert_snapshot!(std::str::from_utf8(primary.to_vec().as_slice()).unwrap());

        let deferred = response.next_response().await.unwrap().unwrap();
        insta::assert_snapshot!(std::str::from_utf8(deferred.to_vec().as_slice()).unwrap());

        // the last deferred response was replace with an empty response,
        // to still have one containing has_next = false
        let last = response.next_response().await.unwrap().unwrap();
        insta::assert_snapshot!(std::str::from_utf8(last.to_vec().as_slice()).unwrap());
    }
}
