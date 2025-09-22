use std::collections::HashMap;
use std::collections::HashSet;
use std::ops::ControlFlow;
use std::time::Duration;

use multimap::MultiMap;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use uuid::Uuid;

use self::callback::CallbackService;
use crate::Endpoint;
use crate::ListenAddr;
use crate::graphql;
use crate::json_ext::Object;
use crate::layers::ServiceBuilderExt;
use crate::notification::Notify;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::protocols::websocket::WebSocketProtocol;
use crate::query_planner::OperationKind;
use crate::register_plugin;
use crate::services::subgraph;

mod callback;
mod execution;

pub(crate) use callback::SUBSCRIPTION_CALLBACK_HMAC_KEY;
pub(crate) use callback::create_verifier;
pub(crate) use execution::SubscriptionExecutionLayer;
pub(crate) use execution::SubscriptionTaskParams;

pub(crate) const APOLLO_SUBSCRIPTION_PLUGIN: &str = "apollo.subscription";
pub(crate) const APOLLO_SUBSCRIPTION_PLUGIN_NAME: &str = "subscription";
pub(crate) const SUBSCRIPTION_ERROR_EXTENSION_KEY: &str = "apollo::subscriptions::fatal_error";
pub(crate) const SUBSCRIPTION_WS_CUSTOM_CONNECTION_PARAMS: &str =
    "apollo.subscription.custom_connection_params";

#[derive(Debug, Clone)]
pub(crate) struct Subscription {
    notify: Notify<String, graphql::Response>,
    callback_hmac_key: Option<String>,
    pub(crate) config: SubscriptionConfig,
}

/// Subscriptions configuration
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SubscriptionConfig {
    /// Enable subscription
    pub(crate) enabled: bool,
    /// Select a subscription mode (callback or passthrough)
    pub(crate) mode: SubscriptionModeConfig,
    /// Configure subgraph subscription deduplication
    pub(crate) deduplication: DeduplicationConfig,
    /// This is a limit to only have maximum X opened subscriptions at the same time. By default if it's not set there is no limit.
    pub(crate) max_opened_subscriptions: Option<usize>,
    /// It represent the capacity of the in memory queue to know how many events we can keep in a buffer
    pub(crate) queue_capacity: Option<usize>,
}

/// Subscription deduplication configuration
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct DeduplicationConfig {
    /// Enable subgraph subscription deduplication. When enabled, multiple identical requests to the same subgraph will share one WebSocket connection in passthrough mode.
    /// (default: true)
    pub(crate) enabled: bool,
    /// List of headers to ignore for deduplication. Even if these headers are different, the subscription request is considered identical.
    /// For example, if you forward the "User-Agent" header, but the subgraph doesn't depend on the value of that header,
    /// adding it to this list will let the router dedupe subgraph subscriptions even if the header value is different.
    pub(crate) ignored_headers: HashSet<String>,
}

impl Default for DeduplicationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            ignored_headers: Default::default(),
        }
    }
}

impl Default for SubscriptionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: Default::default(),
            deduplication: DeduplicationConfig::default(),
            max_opened_subscriptions: None,
            queue_capacity: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubscriptionModeConfig {
    /// Enable callback mode for subgraph(s)
    pub(crate) callback: Option<CallbackMode>,
    /// Enable passthrough mode for subgraph(s)
    pub(crate) passthrough: Option<SubgraphPassthroughMode>,
}

impl SubscriptionModeConfig {
    pub(crate) fn get_subgraph_config(&self, service_name: &str) -> Option<SubscriptionMode> {
        if let Some(passthrough_cfg) = &self.passthrough {
            if let Some(subgraph_cfg) = passthrough_cfg.subgraphs.get(service_name) {
                return SubscriptionMode::Passthrough(subgraph_cfg.clone()).into();
            }
            if let Some(all_cfg) = &passthrough_cfg.all {
                return SubscriptionMode::Passthrough(all_cfg.clone()).into();
            }
        }

        if let Some(callback_cfg) = &self.callback
            && (callback_cfg.subgraphs.contains(service_name) || callback_cfg.subgraphs.is_empty())
        {
            let callback_cfg = CallbackMode {
                public_url: callback_cfg.public_url.clone(),
                heartbeat_interval: callback_cfg.heartbeat_interval,
                listen: callback_cfg.listen.clone(),
                path: callback_cfg.path.clone(),
                subgraphs: HashSet::new(), // We don't need it
            };
            return SubscriptionMode::Callback(callback_cfg).into();
        }

        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Default, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SubgraphPassthroughMode {
    /// Configuration for all subgraphs
    pub(crate) all: Option<WebSocketConfiguration>,
    /// Configuration for specific subgraphs
    pub(crate) subgraphs: HashMap<String, WebSocketConfiguration>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SubscriptionMode {
    /// Using a callback url
    Callback(CallbackMode),
    /// Using websocket to directly connect to subgraph
    Passthrough(WebSocketConfiguration),
}

/// Using a callback url
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CallbackMode {
    #[schemars(with = "String")]
    /// URL used to access this router instance, including the path configured on the Router
    pub(crate) public_url: url::Url,

    /// Heartbeat interval for callback mode (default: 5secs)
    #[serde(default = "HeartbeatInterval::new_enabled")]
    pub(crate) heartbeat_interval: HeartbeatInterval,
    // `skip_serializing` We don't need it in the context
    /// Listen address on which the callback must listen (default: 127.0.0.1:4000)
    #[serde(skip_serializing)]
    pub(crate) listen: Option<ListenAddr>,
    // `skip_serializing` We don't need it in the context
    /// Specify on which path you want to listen for callbacks (default: /callback)
    #[serde(skip_serializing)]
    pub(crate) path: Option<String>,

    /// Specify on which subgraph we enable the callback mode for subscription
    /// If empty it applies to all subgraphs (passthrough mode takes precedence)
    #[serde(default)]
    pub(crate) subgraphs: HashSet<String>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case", untagged)]
pub(crate) enum HeartbeatInterval {
    /// disable heartbeat
    Disabled(Disabled),
    /// enable with default interval of 5s
    Enabled(Enabled),
    /// enable with custom interval, e.g. '100ms', '10s' or '1m'
    #[serde(with = "humantime_serde")]
    #[schemars(with = "String")]
    Duration(Duration),
}

impl HeartbeatInterval {
    pub(crate) fn new_enabled() -> Self {
        Self::Enabled(Enabled::Enabled)
    }
    pub(crate) fn new_disabled() -> Self {
        Self::Disabled(Disabled::Disabled)
    }
    pub(crate) fn into_option(self) -> Option<Duration> {
        match self {
            Self::Disabled(_) => None,
            Self::Enabled(_) => Some(Duration::from_secs(5)),
            Self::Duration(duration) => Some(duration),
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Disabled {
    Disabled,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Enabled {
    Enabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
/// WebSocket configuration for a specific subgraph
pub(crate) struct WebSocketConfiguration {
    /// Path on which WebSockets are listening
    #[serde(default)]
    pub(crate) path: Option<String>,
    /// Which WebSocket GraphQL protocol to use for this subgraph possible values are: 'graphql_ws' | 'graphql_transport_ws' (default: graphql_ws)
    #[serde(default)]
    pub(crate) protocol: WebSocketProtocol,
    /// Heartbeat interval for graphql-ws protocol (default: disabled)
    #[serde(default = "HeartbeatInterval::new_disabled")]
    pub(crate) heartbeat_interval: HeartbeatInterval,
}

fn default_callback_path() -> String {
    String::from("/callback")
}

pub(crate) fn default_listen_addr() -> ListenAddr {
    ListenAddr::SocketAddr("127.0.0.1:4000".parse().expect("valid ListenAddr"))
}

#[async_trait::async_trait]
impl Plugin for Subscription {
    type Config = SubscriptionConfig;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let mut callback_hmac_key = None;
        if init.config.mode.callback.is_some() {
            callback_hmac_key = Some(
                SUBSCRIPTION_CALLBACK_HMAC_KEY
                    .get_or_init(|| Uuid::new_v4().to_string())
                    .clone(),
            );
            #[cfg(not(test))]
            init.notify
                .set_ttl(
                    init.config
                        .mode
                        .callback
                        .as_ref()
                        .expect("we checked in the condition the callback conf")
                        .heartbeat_interval
                        .into_option(),
                )
                .await?;
        }

        Ok(Subscription {
            notify: init.notify,
            callback_hmac_key,
            config: init.config,
        })
    }

    fn subgraph_service(
        &self,
        _subgraph_name: &str,
        service: subgraph::BoxService,
    ) -> subgraph::BoxService {
        let enabled = self.config.enabled
            && (self.config.mode.callback.is_some() || self.config.mode.passthrough.is_some());
        ServiceBuilder::new()
            .checkpoint(move |req: subgraph::Request| {
                if req.operation_kind == OperationKind::Subscription && !enabled {
                    Ok(ControlFlow::Break(
                        subgraph::Response::builder()
                            .context(req.context)
                            .subgraph_name(req.subgraph_name)
                            .error(
                                graphql::Error::builder()
                                    .message("cannot execute a subscription if it's not enabled in the configuration")
                                    .extension_code("SUBSCRIPTION_DISABLED")
                                    .build(),
                            )
                            .extensions(Object::default())
                            .build(),
                    ))
                } else {
                    Ok(ControlFlow::Continue(req))
                }
            })
            .service(service)
            .boxed()
    }

    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
        let mut map = MultiMap::new();

        if let Some(CallbackMode { listen, path, .. }) = &self.config.mode.callback {
            let path = path.clone().unwrap_or_else(default_callback_path);
            let path = path.trim_end_matches('/');
            let callback_hmac_key = self
                .callback_hmac_key
                .clone()
                .expect("cannot run subscription in callback mode without a hmac key");
            let endpoint = Endpoint::from_router_service(
                format!("{path}/{{callback}}"),
                CallbackService::new(self.notify.clone(), path.to_string(), callback_hmac_key)
                    .boxed(),
            );
            map.insert(listen.clone().unwrap_or_else(default_listen_addr), endpoint);
        }

        map
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use std::sync::Arc;

    use futures::StreamExt;
    use http::HeaderName;
    use http::HeaderValue;
    use serde_json::Value;
    use tower::Service;
    use tower::ServiceExt;
    use tower::util::BoxService;

    use super::*;
    use crate::Notify;
    use crate::assert_response_eq_ignoring_error_id;
    use crate::graphql::Request;
    use crate::http_ext;
    use crate::plugin::DynPlugin;
    use crate::plugin::test::MockSubgraphService;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;
    use crate::services::router;
    use crate::services::router::body;
    use crate::uplink::license_enforcement::LicenseState;

    #[tokio::test(flavor = "multi_thread")]
    async fn it_test_callback_endpoint() {
        let mut notify = Notify::builder().build();
        let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
            .find(|factory| factory.name == APOLLO_SUBSCRIPTION_PLUGIN)
            .expect("Plugin not found")
            .create_instance(
                PluginInit::fake_builder()
                    .config(
                        Value::from_str(
                            r#"{
                "enabled": true,
                "mode": {
                    "callback": {
                        "public_url": "http://localhost:4000/subscription/callback",
                        "path": "/subscription/callback",
                        "subgraphs": ["test"]
                    }
                }
            }"#,
                        )
                        .unwrap(),
                    )
                    .notify(notify.clone())
                    .build(),
            )
            .await
            .unwrap();

        let http_req_prom = http::Request::get("http://localhost:4000/subscription/callback")
            .body(body::empty())
            .unwrap();
        let mut web_endpoint = dyn_plugin
            .web_endpoints()
            .into_iter()
            .next()
            .unwrap()
            .1
            .into_iter()
            .next()
            .unwrap()
            .into_router();
        let resp = web_endpoint
            .as_service()
            .ready()
            .await
            .unwrap()
            .call(http_req_prom)
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);
        let new_sub_id = uuid::Uuid::new_v4().to_string();
        let (handler, _created, _) = notify
            .create_or_subscribe(new_sub_id.clone(), true, None)
            .await
            .unwrap();
        let verifier = create_verifier(&new_sub_id).unwrap();
        let http_req = http::Request::post(format!(
            "http://localhost:4000/subscription/callback/{new_sub_id}"
        ))
        .body(router::body::from_bytes(
            serde_json::to_vec(&callback::CallbackPayload::Subscription(
                callback::SubscriptionPayload::Check {
                    id: new_sub_id.clone(),
                    verifier: verifier.clone(),
                },
            ))
            .unwrap(),
        ))
        .unwrap();
        let resp = web_endpoint.clone().oneshot(http_req).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::NO_CONTENT);
        assert_eq!(
            resp.headers()
                .get(HeaderName::from_static(
                    callback::CALLBACK_SUBSCRIPTION_HEADER_NAME
                ))
                .unwrap(),
            HeaderValue::from_static(callback::CALLBACK_SUBSCRIPTION_HEADER_VALUE)
        );

        let http_req = http::Request::post(format!(
            "http://localhost:4000/subscription/callback/{new_sub_id}"
        ))
        .body(router::body::from_bytes(
            serde_json::to_vec(&callback::CallbackPayload::Subscription(callback::SubscriptionPayload::Next {
                id: new_sub_id.clone(),
                payload: Box::new(graphql::Response::builder()
                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                    .build()),
                verifier: verifier.clone(),
            }))
            .unwrap(),
        ))
        .unwrap();
        let resp = web_endpoint.clone().oneshot(http_req).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let mut handler = handler.into_stream();
        let msg = handler.next().await.unwrap();

        assert_eq!(
            msg,
            graphql::Response::builder()
                .subscribed(true)
                .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                .build()
        );
        drop(handler);

        // Should answer NOT FOUND because I dropped the only existing handler and so no one is still listening to the sub
        let http_req = http::Request::post(format!(
            "http://localhost:4000/subscription/callback/{new_sub_id}"
        ))
        .body(router::body::from_bytes(
            serde_json::to_vec(&callback::CallbackPayload::Subscription(callback::SubscriptionPayload::Next {
                id: new_sub_id.clone(),
                payload: Box::new(graphql::Response::builder()
                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                    .build()),
                verifier: verifier.clone(),
            }))
            .unwrap(),
        ))
        .unwrap();
        let resp = web_endpoint.clone().oneshot(http_req).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);

        // Should answer NOT FOUND because I dropped the only existing handler and so no one is still listening to the sub
        let http_req = http::Request::post(format!(
            "http://localhost:4000/subscription/callback/{new_sub_id}"
        ))
        .body(router::body::from_bytes(
            serde_json::to_vec(&callback::CallbackPayload::Subscription(
                callback::SubscriptionPayload::Heartbeat {
                    id: new_sub_id.clone(),
                    ids: vec![new_sub_id, "FAKE_SUB_ID".to_string()],
                    verifier: verifier.clone(),
                },
            ))
            .unwrap(),
        ))
        .unwrap();
        let resp = web_endpoint.oneshot(http_req).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_test_callback_endpoint_with_bad_verifier() {
        let mut notify = Notify::builder().build();
        let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
            .find(|factory| factory.name == APOLLO_SUBSCRIPTION_PLUGIN)
            .expect("Plugin not found")
            .create_instance(
                PluginInit::fake_builder()
                    .config(
                        Value::from_str(
                            r#"{
                        "enabled": true,
                        "mode": {
                            "callback": {
                                "public_url": "http://localhost:4000/subscription/callback",
                                "path": "/subscription/callback",
                                "subgraphs": ["test"]
                            }
                        }
                    }"#,
                        )
                        .unwrap(),
                    )
                    .notify(notify.clone())
                    .license(Arc::new(LicenseState::default()))
                    .build(),
            )
            .await
            .unwrap();

        let http_req_prom = http::Request::get("http://localhost:4000/subscription/callback")
            .body(body::empty())
            .unwrap();
        let mut web_endpoint = dyn_plugin
            .web_endpoints()
            .into_iter()
            .next()
            .unwrap()
            .1
            .into_iter()
            .next()
            .unwrap()
            .into_router();
        let resp = web_endpoint
            .as_service()
            .ready()
            .await
            .unwrap()
            .call(http_req_prom)
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);
        let new_sub_id = uuid::Uuid::new_v4().to_string();
        let (_handler, _created, _) = notify
            .create_or_subscribe(new_sub_id.clone(), true, None)
            .await
            .unwrap();
        let verifier = String::from("XXX");
        let http_req = http::Request::post(format!(
            "http://localhost:4000/subscription/callback/{new_sub_id}"
        ))
        .body(router::body::from_bytes(
            serde_json::to_vec(&callback::CallbackPayload::Subscription(
                callback::SubscriptionPayload::Check {
                    id: new_sub_id.clone(),
                    verifier: verifier.clone(),
                },
            ))
            .unwrap(),
        ))
        .unwrap();
        let resp = web_endpoint.clone().oneshot(http_req).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::UNAUTHORIZED);

        let http_req = http::Request::post(format!(
            "http://localhost:4000/subscription/callback/{new_sub_id}"
        ))
        .body(router::body::from_bytes(
            serde_json::to_vec(&callback::CallbackPayload::Subscription(callback::SubscriptionPayload::Next {
                id: new_sub_id.clone(),
                payload: Box::new(graphql::Response::builder()
                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                    .build()),
                verifier: verifier.clone(),
            }))
            .unwrap(),
        ))
        .unwrap();
        let resp = web_endpoint.clone().oneshot(http_req).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_test_callback_endpoint_with_complete_subscription() {
        let mut notify = Notify::builder().build();
        let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
            .find(|factory| factory.name == APOLLO_SUBSCRIPTION_PLUGIN)
            .expect("Plugin not found")
            .create_instance(
                PluginInit::fake_builder()
                    .config(
                        Value::from_str(
                            r#"{
                "enabled": true,
                "mode": {
                    "callback": {
                        "public_url": "http://localhost:4000/subscription/callback",
                        "path": "/subscription/callback",
                        "subgraphs": ["test"]
                    }
                }
            }"#,
                        )
                        .unwrap(),
                    )
                    .notify(notify.clone())
                    .license(Arc::new(LicenseState::default()))
                    .build(),
            )
            .await
            .unwrap();

        let http_req_prom = http::Request::get("http://localhost:4000/subscription/callback")
            .body(body::empty())
            .unwrap();
        let mut web_endpoint = dyn_plugin
            .web_endpoints()
            .into_iter()
            .next()
            .unwrap()
            .1
            .into_iter()
            .next()
            .unwrap()
            .into_router();
        let resp = web_endpoint
            .as_service()
            .ready()
            .await
            .unwrap()
            .call(http_req_prom)
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);
        let new_sub_id = uuid::Uuid::new_v4().to_string();
        let (handler, _created, _) = notify
            .create_or_subscribe(new_sub_id.clone(), true, None)
            .await
            .unwrap();
        let verifier = create_verifier(&new_sub_id).unwrap();

        let http_req = http::Request::post(format!(
            "http://localhost:4000/subscription/callback/{new_sub_id}"
        ))
        .body(router::body::from_bytes(
            serde_json::to_vec(&callback::CallbackPayload::Subscription(
                callback::SubscriptionPayload::Check {
                    id: new_sub_id.clone(),
                    verifier: verifier.clone(),
                },
            ))
            .unwrap(),
        ))
        .unwrap();
        let resp = web_endpoint.clone().oneshot(http_req).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::NO_CONTENT);
        assert_eq!(
            resp.headers()
                .get(HeaderName::from_static(
                    callback::CALLBACK_SUBSCRIPTION_HEADER_NAME
                ))
                .unwrap(),
            HeaderValue::from_static(callback::CALLBACK_SUBSCRIPTION_HEADER_VALUE)
        );

        let http_req = http::Request::post(format!(
            "http://localhost:4000/subscription/callback/{new_sub_id}"
        ))
        .body(router::body::from_bytes(
            serde_json::to_vec(&callback::CallbackPayload::Subscription(callback::SubscriptionPayload::Next {
                id: new_sub_id.clone(),
                payload: Box::new(graphql::Response::builder()
                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                    .build()),
                verifier: verifier.clone(),
            }))
            .unwrap(),
        ))
        .unwrap();
        let resp = web_endpoint.clone().oneshot(http_req).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let mut handler = handler.into_stream();
        let msg = handler.next().await.unwrap();

        assert_eq!(
            msg,
            graphql::Response::builder()
                .subscribed(true)
                .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                .build()
        );

        let http_req = http::Request::post(format!(
            "http://localhost:4000/subscription/callback/{new_sub_id}"
        ))
        .body(router::body::from_bytes(
            serde_json::to_vec(&callback::CallbackPayload::Subscription(
                callback::SubscriptionPayload::Complete {
                    id: new_sub_id.clone(),
                    errors: Some(vec![
                        graphql::Error::builder()
                            .message("cannot complete the subscription")
                            .extension_code("SUBSCRIPTION_ERROR")
                            .build(),
                    ]),
                    verifier: verifier.clone(),
                },
            ))
            .unwrap(),
        ))
        .unwrap();
        let resp = web_endpoint.clone().oneshot(http_req).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::ACCEPTED);
        let msg = handler.next().await.unwrap();
        assert_response_eq_ignoring_error_id!(
            msg,
            graphql::Response::builder()
                .errors(vec![
                    graphql::Error::builder()
                        .message("cannot complete the subscription")
                        .extension_code("SUBSCRIPTION_ERROR")
                        .build()
                ])
                .build()
        );

        // Should answer NOT FOUND because we completed the sub
        let http_req = http::Request::post(format!(
            "http://localhost:4000/subscription/callback/{new_sub_id}"
        ))
        .body(router::body::from_bytes(
            serde_json::to_vec(&callback::CallbackPayload::Subscription(callback::SubscriptionPayload::Next {
                id: new_sub_id.clone(),
                payload: Box::new(graphql::Response::builder()
                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                    .build()),
                verifier,
            }))
            .unwrap(),
        ))
        .unwrap();
        let resp = web_endpoint.oneshot(http_req).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_test_subgraph_service_with_subscription_disabled() {
        let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
            .find(|factory| factory.name == APOLLO_SUBSCRIPTION_PLUGIN)
            .expect("Plugin not found")
            .create_instance_without_schema(
                &Value::from_str(
                    r#"{
                    "enabled": false
                }"#,
                )
                .unwrap(),
            )
            .await
            .unwrap();

        let mut mock_subgraph_service = MockSubgraphService::new();
        mock_subgraph_service
            .expect_call()
            .times(0)
            .returning(move |req: SubgraphRequest| {
                Ok(SubgraphResponse::fake_builder()
                    .context(req.context)
                    .build())
            });

        let mut subgraph_service =
            dyn_plugin.subgraph_service("my_subgraph_name", BoxService::new(mock_subgraph_service));
        let subgraph_req = SubgraphRequest::fake_builder()
            .subgraph_request(
                http_ext::Request::fake_builder()
                    .body(
                        Request::fake_builder()
                            .query(String::from(
                                "subscription {\n  userWasCreated {\n    username\n  }\n}",
                            ))
                            .build(),
                    )
                    .build()
                    .unwrap(),
            )
            .operation_kind(OperationKind::Subscription)
            .build();
        let subgraph_response = subgraph_service
            .ready()
            .await
            .unwrap()
            .call(subgraph_req)
            .await
            .unwrap();

        assert_response_eq_ignoring_error_id!(
            subgraph_response.response.body(),
            &graphql::Response::builder()
                .data(serde_json_bytes::Value::Null)
                .error(
                    graphql::Error::builder()
                        .message(
                            "cannot execute a subscription if it's not enabled in the configuration"
                        )
                        .extension_code("SUBSCRIPTION_DISABLED")
                        .build()
                )
                .extensions(Object::default())
                .build()
        );
    }

    #[test]
    fn it_test_subscription_config() {
        let config_with_callback: SubscriptionConfig = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "mode": {
                "callback": {
                    "public_url": "http://localhost:4000/subscription/callback",
                    "path": "/subscription/callback",
                    "subgraphs": ["test"]
                }
            }
        }))
        .unwrap();

        let subgraph_cfg = config_with_callback.mode.get_subgraph_config("test");
        assert_eq!(
            subgraph_cfg,
            Some(SubscriptionMode::Callback(
                serde_json::from_value::<CallbackMode>(serde_json::json!({
                    "public_url": "http://localhost:4000/subscription/callback",
                    "path": "/subscription/callback",
                    "subgraphs": []
                }))
                .unwrap()
            ))
        );

        let config_with_callback_default: SubscriptionConfig =
            serde_json::from_value(serde_json::json!({
                "enabled": true,
                "mode": {
                    "callback": {
                        "public_url": "http://localhost:4000/subscription/callback",
                        "path": "/subscription/callback",
                    }
                }
            }))
            .unwrap();

        let subgraph_cfg = config_with_callback_default
            .mode
            .get_subgraph_config("test");
        assert_eq!(
            subgraph_cfg,
            Some(SubscriptionMode::Callback(
                serde_json::from_value::<CallbackMode>(serde_json::json!({
                    "public_url": "http://localhost:4000/subscription/callback",
                    "path": "/subscription/callback",
                    "subgraphs": []
                }))
                .unwrap()
            ))
        );

        let config_with_passthrough: SubscriptionConfig =
            serde_json::from_value(serde_json::json!({
                "enabled": true,
                "mode": {
                    "passthrough": {
                        "subgraphs": {
                            "test": {
                                "path": "/ws",
                            }
                        }
                    }
                }
            }))
            .unwrap();

        let subgraph_cfg = config_with_passthrough.mode.get_subgraph_config("test");
        assert_eq!(
            subgraph_cfg,
            Some(SubscriptionMode::Passthrough(
                serde_json::from_value::<WebSocketConfiguration>(serde_json::json!({
                    "path": "/ws",
                }))
                .unwrap()
            ))
        );

        let config_with_passthrough_override: SubscriptionConfig =
            serde_json::from_value(serde_json::json!({
                "enabled": true,
                "mode": {
                    "passthrough": {
                        "all": {
                            "path": "/wss",
                            "protocol": "graphql_transport_ws"
                        },
                        "subgraphs": {
                            "test": {
                                "path": "/ws",
                            }
                        }
                    }
                }
            }))
            .unwrap();

        let subgraph_cfg = config_with_passthrough_override
            .mode
            .get_subgraph_config("test");
        assert_eq!(
            subgraph_cfg,
            Some(SubscriptionMode::Passthrough(
                serde_json::from_value::<WebSocketConfiguration>(serde_json::json!({
                    "path": "/ws",
                    "protocol": "graphql_ws"
                }))
                .unwrap()
            ))
        );

        let config_with_passthrough_all: SubscriptionConfig =
            serde_json::from_value(serde_json::json!({
                "enabled": true,
                "mode": {
                    "passthrough": {
                        "all": {
                            "path": "/wss",
                            "protocol": "graphql_transport_ws"
                        },
                        "subgraphs": {
                            "foo": {
                                "path": "/ws",
                            }
                        }
                    }
                }
            }))
            .unwrap();

        let subgraph_cfg = config_with_passthrough_all.mode.get_subgraph_config("test");
        assert_eq!(
            subgraph_cfg,
            Some(SubscriptionMode::Passthrough(
                serde_json::from_value::<WebSocketConfiguration>(serde_json::json!({
                    "path": "/wss",
                    "protocol": "graphql_transport_ws"
                }))
                .unwrap()
            ))
        );

        let config_with_both_mode: SubscriptionConfig = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "mode": {
                "callback": {
                    "public_url": "http://localhost:4000/subscription/callback",
                    "path": "/subscription/callback",
                },
                "passthrough": {
                    "subgraphs": {
                        "foo": {
                            "path": "/ws",
                        }
                    }
                }
            }
        }))
        .unwrap();

        let subgraph_cfg = config_with_both_mode.mode.get_subgraph_config("test");
        assert_eq!(
            subgraph_cfg,
            Some(SubscriptionMode::Callback(
                serde_json::from_value::<CallbackMode>(serde_json::json!({
                    "public_url": "http://localhost:4000/subscription/callback",
                    "path": "/subscription/callback",
                }))
                .unwrap()
            ))
        );

        let config_with_passthrough_precedence: SubscriptionConfig =
            serde_json::from_value(serde_json::json!({
                "enabled": true,
                "mode": {
                    "callback": {
                        "public_url": "http://localhost:4000/subscription/callback",
                        "path": "/subscription/callback",
                    },
                    "passthrough": {
                        "all": {
                            "path": "/wss",
                            "protocol": "graphql_transport_ws"
                        },
                        "subgraphs": {
                            "foo": {
                                "path": "/ws",
                            }
                        }
                    }
                }
            }))
            .unwrap();

        let subgraph_cfg = config_with_passthrough_precedence
            .mode
            .get_subgraph_config("test");
        assert_eq!(
            subgraph_cfg,
            Some(SubscriptionMode::Passthrough(
                serde_json::from_value::<WebSocketConfiguration>(serde_json::json!({
                    "path": "/wss",
                    "protocol": "graphql_transport_ws"
                }))
                .unwrap()
            ))
        );

        let config_without_mode: SubscriptionConfig = serde_json::from_value(serde_json::json!({
            "enabled": true
        }))
        .unwrap();

        let subgraph_cfg = config_without_mode.mode.get_subgraph_config("test");
        assert_eq!(subgraph_cfg, None);

        let sub_config: SubscriptionConfig = serde_json::from_value(serde_json::json!({
            "mode": {
                "callback": {
                    "public_url": "http://localhost:4000/subscription/callback",
                    "path": "/subscription/callback",
                    "subgraphs": ["test"]
                }
            }
        }))
        .unwrap();

        assert!(sub_config.enabled);
        assert!(sub_config.deduplication.enabled);
        assert!(sub_config.max_opened_subscriptions.is_none());
        assert!(sub_config.queue_capacity.is_none());
    }
}

register_plugin!("apollo", "subscription", Subscription);
