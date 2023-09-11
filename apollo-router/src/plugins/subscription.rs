use std::collections::HashMap;
use std::collections::HashSet;
use std::ops::ControlFlow;
use std::task::Poll;

use bytes::Buf;
use futures::future::BoxFuture;
use hmac::Hmac;
use hmac::Mac;
use http::Method;
use http::StatusCode;
use multimap::MultiMap;
use once_cell::sync::OnceCell;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use tower::BoxError;
use tower::Service;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tracing_futures::Instrument;
use uuid::Uuid;

use crate::context::Context;
use crate::graphql;
use crate::graphql::Response;
use crate::json_ext::Object;
use crate::layers::ServiceBuilderExt;
use crate::notification::Notify;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::protocols::websocket::WebSocketProtocol;
use crate::query_planner::OperationKind;
use crate::register_plugin;
use crate::services::router;
use crate::services::subgraph;
use crate::Endpoint;
use crate::ListenAddr;

type HmacSha256 = Hmac<sha2::Sha256>;
pub(crate) const APOLLO_SUBSCRIPTION_PLUGIN: &str = "apollo.subscription";
#[cfg(not(test))]
pub(crate) const APOLLO_SUBSCRIPTION_PLUGIN_NAME: &str = "subscription";
pub(crate) static SUBSCRIPTION_CALLBACK_HMAC_KEY: OnceCell<String> = OnceCell::new();
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
    /// Enable the deduplication of subscription (for example if we detect the exact same request to subgraph we won't open a new websocket to the subgraph in passthrough mode)
    /// (default: true)
    #[serde(default = "enable_deduplication_default")]
    pub(crate) enable_deduplication: bool,
    /// This is a limit to only have maximum X opened subscriptions at the same time. By default if it's not set there is no limit.
    pub(crate) max_opened_subscriptions: Option<usize>,
    /// It represent the capacity of the in memory queue to know how many events we can keep in a buffer
    pub(crate) queue_capacity: Option<usize>,
}

fn enable_deduplication_default() -> bool {
    true
}

impl Default for SubscriptionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: Default::default(),
            enable_deduplication: enable_deduplication_default(),
            max_opened_subscriptions: None,
            queue_capacity: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubscriptionModeConfig {
    #[serde(rename = "preview_callback")]
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

        if let Some(callback_cfg) = &self.callback {
            if callback_cfg.subgraphs.contains(service_name) || callback_cfg.subgraphs.is_empty() {
                let callback_cfg = CallbackMode {
                    public_url: callback_cfg.public_url.clone(),
                    listen: callback_cfg.listen.clone(),
                    path: callback_cfg.path.clone(),
                    subgraphs: HashSet::new(), // We don't need it
                };
                return SubscriptionMode::Callback(callback_cfg).into();
            }
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
    /// URL used to access this router instance
    pub(crate) public_url: url::Url,
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

/// Using websocket to directly connect to subgraph
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct PassthroughMode {
    /// WebSocket configuration for specific subgraphs
    subgraph: SubgraphPassthroughMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
/// WebSocket configuration for a specific subgraph
pub(crate) struct WebSocketConfiguration {
    /// Path on which WebSockets are listening
    pub(crate) path: Option<String>,
    /// Which WebSocket GraphQL protocol to use for this subgraph possible values are: 'graphql_ws' | 'graphql_transport_ws' (default: graphql_ws)
    pub(crate) protocol: WebSocketProtocol,
}

fn default_path() -> String {
    String::from("/callback")
}

fn default_listen_addr() -> ListenAddr {
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
                    Ok(ControlFlow::Break(subgraph::Response::builder().context(req.context).error(graphql::Error::builder().message("cannot execute a subscription if it's not enabled in the configuration").extension_code("SUBSCRIPTION_DISABLED").build()).extensions(Object::default()).build()))
                } else {
                    Ok(ControlFlow::Continue(req))
                }
            }).service(service)
            .boxed()
    }

    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
        let mut map = MultiMap::new();

        if let Some(CallbackMode { listen, path, .. }) = &self.config.mode.callback {
            let path = path.clone().unwrap_or_else(default_path);
            let path = path.trim_end_matches('/');
            let callback_hmac_key = self
                .callback_hmac_key
                .clone()
                .expect("cannot run subscription in callback mode without a hmac key");
            let endpoint = Endpoint::from_router_service(
                format!("{path}/:callback"),
                CallbackService::new(self.notify.clone(), path.to_string(), callback_hmac_key)
                    .boxed(),
            );
            map.insert(listen.clone().unwrap_or_else(default_listen_addr), endpoint);
        }

        map
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "kind", rename = "lowercase")]
pub(crate) enum CallbackPayload {
    #[serde(rename = "subscription")]
    Subscription(SubscriptionPayload),
}

impl CallbackPayload {
    fn id(&self) -> &String {
        match self {
            CallbackPayload::Subscription(subscription_payload) => subscription_payload.id(),
        }
    }

    fn verifier(&self) -> &String {
        match self {
            CallbackPayload::Subscription(subscription_payload) => subscription_payload.verifier(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
/// Callback payload when a subscription id is incorrect
pub(crate) struct InvalidIdsPayload {
    /// List of invalid ids
    pub(crate) invalid_ids: Vec<String>,
    pub(crate) id: String,
    pub(crate) verifier: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "action", rename = "lowercase")]
pub(crate) enum SubscriptionPayload {
    #[serde(rename = "check")]
    Check { id: String, verifier: String },
    #[serde(rename = "heartbeat")]
    Heartbeat {
        /// Id sent with the corresponding verifier
        id: String,
        /// List of ids to heartbeat
        ids: Vec<String>,
        /// Verifier received with the corresponding id
        verifier: String,
    },
    #[serde(rename = "next")]
    Next {
        id: String,
        payload: Response,
        verifier: String,
    },
    #[serde(rename = "complete")]
    Complete {
        id: String,
        verifier: String,
        errors: Option<Vec<graphql::Error>>,
    },
}

impl SubscriptionPayload {
    fn id(&self) -> &String {
        match self {
            SubscriptionPayload::Check { id, .. }
            | SubscriptionPayload::Heartbeat { id, .. }
            | SubscriptionPayload::Next { id, .. }
            | SubscriptionPayload::Complete { id, .. } => id,
        }
    }

    fn verifier(&self) -> &String {
        match self {
            SubscriptionPayload::Check { verifier, .. }
            | SubscriptionPayload::Heartbeat { verifier, .. }
            | SubscriptionPayload::Next { verifier, .. }
            | SubscriptionPayload::Complete { verifier, .. } => verifier,
        }
    }
}

#[derive(Clone)]
pub(crate) struct CallbackService {
    notify: Notify<String, graphql::Response>,
    path: String,
    callback_hmac_key: String,
}

impl CallbackService {
    pub(crate) fn new(
        notify: Notify<String, graphql::Response>,
        path: String,
        callback_hmac_key: String,
    ) -> Self {
        Self {
            notify,
            path,
            callback_hmac_key,
        }
    }
}

impl Service<router::Request> for CallbackService {
    type Response = router::Response;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, req: router::Request) -> Self::Future {
        let mut notify = self.notify.clone();
        let path = self.path.clone();
        let callback_hmac_key = self.callback_hmac_key.clone();
        Box::pin(
            async move {
                let (parts, body) = req.router_request.into_parts();
                let sub_id = parts
                    .uri
                    .path()
                    .trim_start_matches(&format!("{path}/"))
                    .to_string();

                match parts.method {
                    Method::POST => {
                        let cb_body = hyper::body::to_bytes(body)
                            .await
                            .map_err(|e| format!("failed to get the request body: {e}"))
                            .and_then(|bytes| {
                                serde_json::from_reader::<_, CallbackPayload>(bytes.reader())
                                    .map_err(|err| {
                                        format!(
                                        "failed to deserialize the request body into JSON: {err}"
                                    )
                                    })
                            });
                        let cb_body = match cb_body {
                            Ok(cb_body) => cb_body,
                            Err(err) => {
                                return Ok(router::Response {
                                    response: http::Response::builder()
                                        .status(StatusCode::BAD_REQUEST)
                                        .body(err.into())
                                        .map_err(BoxError::from)?,
                                    context: req.context,
                                });
                            }
                        };
                        let id = cb_body.id().clone();

                        // Hash verifier to sha256 to mitigate timing attack
                        // Check verifier
                        let verifier = cb_body.verifier();
                        let mut verifier_hasher = Sha256::new();
                        verifier_hasher.update(verifier.as_bytes());
                        let hashed_verifier = verifier_hasher.finalize();

                        let mut mac = HmacSha256::new_from_slice(callback_hmac_key.as_bytes())?;
                        mac.update(id.as_bytes());
                        let result = mac.finalize();
                        let expected_verifier = hex::encode(result.into_bytes());
                        let mut verifier_hasher = Sha256::new();
                        verifier_hasher.update(expected_verifier.as_bytes());
                        let expected_hashed_verifier = verifier_hasher.finalize();

                        if hashed_verifier != expected_hashed_verifier {
                            return Ok(router::Response {
                                response: http::Response::builder()
                                    .status(StatusCode::UNAUTHORIZED)
                                    .body("verifier doesn't match".into())
                                    .map_err(BoxError::from)?,
                                context: req.context,
                            });
                        }

                        if let Err(res) = ensure_id_consistency(&req.context, &sub_id, &id) {
                            return Ok(res);
                        }

                        match cb_body {
                            CallbackPayload::Subscription(SubscriptionPayload::Next {
                                mut payload,
                                ..
                            }) => {
                                let mut handle = match notify.subscribe_if_exist(id).await? {
                                    Some(handle) => handle.into_sink(),
                                    None => {
                                        return Ok(router::Response {
                                            response: http::Response::builder()
                                                .status(StatusCode::NOT_FOUND)
                                                .body("suscription doesn't exist".into())
                                                .map_err(BoxError::from)?,
                                            context: req.context,
                                        });
                                    }
                                };
                                // Keep the subscription to the client opened
                                payload.subscribed = Some(true);
                                tracing::info!(
                                        monotonic_counter.apollo.router.operations.subscriptions.events = 1u64,
                                        subscriptions.mode="callback"
                                    );
                                handle.send_sync(payload)?;

                                Ok(router::Response {
                                    response: http::Response::builder()
                                        .status(StatusCode::OK)
                                        .body::<hyper::Body>("".into())
                                        .map_err(BoxError::from)?,
                                    context: req.context,
                                })
                            }
                            CallbackPayload::Subscription(SubscriptionPayload::Check {
                                ..
                            }) => {
                                if notify.exist(id).await? {
                                    Ok(router::Response {
                                        response: http::Response::builder()
                                            .status(StatusCode::NO_CONTENT)
                                            .body::<hyper::Body>("".into())
                                            .map_err(BoxError::from)?,
                                        context: req.context,
                                    })
                                } else {
                                    Ok(router::Response {
                                        response: http::Response::builder()
                                            .status(StatusCode::NOT_FOUND)
                                            .body("suscription doesn't exist".into())
                                            .map_err(BoxError::from)?,
                                        context: req.context,
                                    })
                                }
                            }
                            CallbackPayload::Subscription(SubscriptionPayload::Heartbeat {
                                ids,
                                id,
                                verifier,
                            }) => {
                                if !ids.contains(&id) {
                                    return Ok(router::Response {
                                        response: http::Response::builder()
                                            .status(StatusCode::UNAUTHORIZED)
                                            .body("id used for the verifier is not part of ids array".into())
                                            .map_err(BoxError::from)?,
                                        context: req.context,
                                    });
                                }

                                let (mut valid_ids, invalid_ids) = notify.invalid_ids(ids).await?;
                                if invalid_ids.is_empty() {
                                    Ok(router::Response {
                                        response: http::Response::builder()
                                            .status(StatusCode::NO_CONTENT)
                                            .body::<hyper::Body>("".into())
                                            .map_err(BoxError::from)?,
                                        context: req.context,
                                    })
                                } else if valid_ids.is_empty() {
                                    Ok(router::Response {
                                        response: http::Response::builder()
                                            .status(StatusCode::NOT_FOUND)
                                            .body("suscriptions don't exist".into())
                                            .map_err(BoxError::from)?,
                                        context: req.context,
                                    })
                                } else {
                                    let (id, verifier) = if invalid_ids.contains(&id) {
                                        (id, verifier)
                                    } else {
                                        let new_id = valid_ids.pop().expect("valid_ids is not empty, checked in the previous if block");
                                        // Generate new verifier
                                        let mut mac = HmacSha256::new_from_slice(
                                            callback_hmac_key.as_bytes(),
                                        )?;
                                        mac.update(new_id.as_bytes());
                                        let result = mac.finalize();
                                        let verifier = hex::encode(result.into_bytes());

                                        (new_id, verifier)
                                    };
                                    Ok(router::Response {
                                        response: http::Response::builder()
                                            .status(StatusCode::NOT_FOUND)
                                            .body(serde_json::to_string_pretty(&InvalidIdsPayload{
                                                invalid_ids,
                                                id,
                                                verifier,
                                            })?.into())
                                            .map_err(BoxError::from)?,
                                        context: req.context,
                                    })
                                }
                            }
                            CallbackPayload::Subscription(SubscriptionPayload::Complete {
                                errors,
                                ..
                            }) => {
                                if let Some(errors) = errors {
                                    let mut handle =
                                        notify.subscribe(id.clone()).await?.into_sink();
                                    tracing::info!(
                                        monotonic_counter.apollo.router.operations.subscriptions.events = 1u64,
                                        subscriptions.mode="callback",
                                        subscriptions.complete=true
                                    );
                                    handle.send_sync(
                                        graphql::Response::builder().errors(errors).build(),
                                    )?;
                                }
                                notify.force_delete(id).await?;
                                Ok(router::Response {
                                    response: http::Response::builder()
                                        .status(StatusCode::ACCEPTED)
                                        .body::<hyper::Body>("".into())
                                        .map_err(BoxError::from)?,
                                    context: req.context,
                                })
                            }
                        }
                    }
                    _ => Ok(router::Response {
                        response: http::Response::builder()
                            .status(StatusCode::METHOD_NOT_ALLOWED)
                            .body::<hyper::Body>("".into())
                            .map_err(BoxError::from)?,
                        context: req.context,
                    }),
                }
            }
            .instrument(tracing::info_span!("subscription_callback")),
        )
    }
}

pub(crate) fn create_verifier(sub_id: &str) -> Result<String, BoxError> {
    let callback_hmac_key = SUBSCRIPTION_CALLBACK_HMAC_KEY
        .get()
        .ok_or("subscription callback hmac key is not available")?;
    let mut mac = HmacSha256::new_from_slice(callback_hmac_key.as_bytes())?;
    mac.update(sub_id.as_bytes());
    let result = mac.finalize();
    let verifier = hex::encode(result.into_bytes());

    Ok(verifier)
}

fn ensure_id_consistency(
    context: &Context,
    id_from_path: &str,
    id_from_body: &str,
) -> Result<(), router::Response> {
    (id_from_path != id_from_body)
        .then(|| {
            Err(router::Response {
                response: http::Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body::<hyper::Body>("id from url path and id from body are different".into())
                    .expect("this body is valid"),
                context: context.clone(),
            })
        })
        .unwrap_or_else(|| Ok(()))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use futures::StreamExt;
    use serde_json::Value;
    use tower::util::BoxService;
    use tower::Service;
    use tower::ServiceExt;

    use super::*;
    use crate::graphql::Request;
    use crate::http_ext;
    use crate::plugin::test::MockSubgraphService;
    use crate::plugin::DynPlugin;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;
    use crate::Notify;

    #[tokio::test(flavor = "multi_thread")]
    async fn it_test_callback_endpoint() {
        let mut notify = Notify::builder().build();
        let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
            .find(|factory| factory.name == APOLLO_SUBSCRIPTION_PLUGIN)
            .expect("Plugin not found")
            .create_instance(
                &Value::from_str(
                    r#"{
                "enabled": true,
                "mode": {
                    "preview_callback": {
                        "public_url": "http://localhost:4000",
                        "path": "/subscription/callback",
                        "subgraphs": ["test"]
                    }
                }
            }"#,
                )
                .unwrap(),
                Default::default(),
                notify.clone(),
            )
            .await
            .unwrap();

        let http_req_prom = http::Request::get("http://localhost:4000/subscription/callback")
            .body(Default::default())
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
            .ready()
            .await
            .unwrap()
            .call(http_req_prom)
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);
        let new_sub_id = uuid::Uuid::new_v4().to_string();
        let (handler, _created) = notify
            .create_or_subscribe(new_sub_id.clone(), true)
            .await
            .unwrap();
        let verifier = create_verifier(&new_sub_id).unwrap();
        let http_req = http::Request::post(format!(
            "http://localhost:4000/subscription/callback/{new_sub_id}"
        ))
        .body(hyper::Body::from(
            serde_json::to_vec(&CallbackPayload::Subscription(SubscriptionPayload::Check {
                id: new_sub_id.clone(),
                verifier: verifier.clone(),
            }))
            .unwrap(),
        ))
        .unwrap();
        let resp = web_endpoint.clone().oneshot(http_req).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::NO_CONTENT);

        let http_req = http::Request::post(format!(
            "http://localhost:4000/subscription/callback/{new_sub_id}"
        ))
        .body(hyper::Body::from(
            serde_json::to_vec(&CallbackPayload::Subscription(SubscriptionPayload::Next {
                id: new_sub_id.clone(),
                payload: graphql::Response::builder()
                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                    .build(),
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
        .body(hyper::Body::from(
            serde_json::to_vec(&CallbackPayload::Subscription(SubscriptionPayload::Next {
                id: new_sub_id.clone(),
                payload: graphql::Response::builder()
                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                    .build(),
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
        .body(hyper::Body::from(
            serde_json::to_vec(&CallbackPayload::Subscription(
                SubscriptionPayload::Heartbeat {
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
                &Value::from_str(
                    r#"{
                "enabled": true,
                "mode": {
                    "preview_callback": {
                        "public_url": "http://localhost:4000",
                        "path": "/subscription/callback",
                        "subgraphs": ["test"]
                    }
                }
            }"#,
                )
                .unwrap(),
                Default::default(),
                notify.clone(),
            )
            .await
            .unwrap();

        let http_req_prom = http::Request::get("http://localhost:4000/subscription/callback")
            .body(Default::default())
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
            .ready()
            .await
            .unwrap()
            .call(http_req_prom)
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);
        let new_sub_id = uuid::Uuid::new_v4().to_string();
        let (_handler, _created) = notify
            .create_or_subscribe(new_sub_id.clone(), true)
            .await
            .unwrap();
        let verifier = String::from("XXX");
        let http_req = http::Request::post(format!(
            "http://localhost:4000/subscription/callback/{new_sub_id}"
        ))
        .body(hyper::Body::from(
            serde_json::to_vec(&CallbackPayload::Subscription(SubscriptionPayload::Check {
                id: new_sub_id.clone(),
                verifier: verifier.clone(),
            }))
            .unwrap(),
        ))
        .unwrap();
        let resp = web_endpoint.clone().oneshot(http_req).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::UNAUTHORIZED);

        let http_req = http::Request::post(format!(
            "http://localhost:4000/subscription/callback/{new_sub_id}"
        ))
        .body(hyper::Body::from(
            serde_json::to_vec(&CallbackPayload::Subscription(SubscriptionPayload::Next {
                id: new_sub_id.clone(),
                payload: graphql::Response::builder()
                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                    .build(),
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
                &Value::from_str(
                    r#"{
                "enabled": true,
                "mode": {
                    "preview_callback": {
                        "public_url": "http://localhost:4000",
                        "path": "/subscription/callback",
                        "subgraphs": ["test"]
                    }
                }
            }"#,
                )
                .unwrap(),
                Default::default(),
                notify.clone(),
            )
            .await
            .unwrap();

        let http_req_prom = http::Request::get("http://localhost:4000/subscription/callback")
            .body(Default::default())
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
            .ready()
            .await
            .unwrap()
            .call(http_req_prom)
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);
        let new_sub_id = uuid::Uuid::new_v4().to_string();
        let (handler, _created) = notify
            .create_or_subscribe(new_sub_id.clone(), true)
            .await
            .unwrap();
        let verifier = create_verifier(&new_sub_id).unwrap();

        let http_req = http::Request::post(format!(
            "http://localhost:4000/subscription/callback/{new_sub_id}"
        ))
        .body(hyper::Body::from(
            serde_json::to_vec(&CallbackPayload::Subscription(SubscriptionPayload::Check {
                id: new_sub_id.clone(),
                verifier: verifier.clone(),
            }))
            .unwrap(),
        ))
        .unwrap();
        let resp = web_endpoint.clone().oneshot(http_req).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::NO_CONTENT);

        let http_req = http::Request::post(format!(
            "http://localhost:4000/subscription/callback/{new_sub_id}"
        ))
        .body(hyper::Body::from(
            serde_json::to_vec(&CallbackPayload::Subscription(SubscriptionPayload::Next {
                id: new_sub_id.clone(),
                payload: graphql::Response::builder()
                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                    .build(),
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
        .body(hyper::Body::from(
            serde_json::to_vec(&CallbackPayload::Subscription(
                SubscriptionPayload::Complete {
                    id: new_sub_id.clone(),
                    errors: Some(vec![graphql::Error::builder()
                        .message("cannot complete the subscription")
                        .extension_code("SUBSCRIPTION_ERROR")
                        .build()]),
                    verifier: verifier.clone(),
                },
            ))
            .unwrap(),
        ))
        .unwrap();
        let resp = web_endpoint.clone().oneshot(http_req).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::ACCEPTED);
        let msg = handler.next().await.unwrap();

        assert_eq!(
            msg,
            graphql::Response::builder()
                .errors(vec![graphql::Error::builder()
                    .message("cannot complete the subscription")
                    .extension_code("SUBSCRIPTION_ERROR")
                    .build()])
                .build()
        );

        // Should answer NOT FOUND because we completed the sub
        let http_req = http::Request::post(format!(
            "http://localhost:4000/subscription/callback/{new_sub_id}"
        ))
        .body(hyper::Body::from(
            serde_json::to_vec(&CallbackPayload::Subscription(SubscriptionPayload::Next {
                id: new_sub_id.clone(),
                payload: graphql::Response::builder()
                    .data(serde_json_bytes::json!({"userWasCreated": {"username": "ada_lovelace"}}))
                    .build(),
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
            .create_instance(
                &Value::from_str(
                    r#"{
                    "enabled": false
                }"#,
                )
                .unwrap(),
                Default::default(),
                Default::default(),
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

        assert_eq!(subgraph_response.response.body(), &graphql::Response::builder().data(serde_json_bytes::Value::Null).error(graphql::Error::builder().message("cannot execute a subscription if it's not enabled in the configuration").extension_code("SUBSCRIPTION_DISABLED").build()).extensions(Object::default()).build());
    }

    #[test]
    fn it_test_subscription_config() {
        let config_with_callback: SubscriptionConfig = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "mode": {
                "preview_callback": {
                    "public_url": "http://localhost:4000",
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
                    "public_url": "http://localhost:4000",
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
                    "preview_callback": {
                        "public_url": "http://localhost:4000",
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
                    "public_url": "http://localhost:4000",
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
                "preview_callback": {
                    "public_url": "http://localhost:4000",
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
                    "public_url": "http://localhost:4000",
                    "path": "/subscription/callback",
                }))
                .unwrap()
            ))
        );

        let config_with_passthrough_precedence: SubscriptionConfig =
            serde_json::from_value(serde_json::json!({
                "enabled": true,
                "mode": {
                    "preview_callback": {
                        "public_url": "http://localhost:4000",
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
                "preview_callback": {
                    "public_url": "http://localhost:4000",
                    "path": "/subscription/callback",
                    "subgraphs": ["test"]
                }
            }
        }))
        .unwrap();

        assert!(sub_config.enabled);
        assert!(sub_config.enable_deduplication);
        assert!(sub_config.max_opened_subscriptions.is_none());
        assert!(sub_config.queue_capacity.is_none());
    }
}

register_plugin!("apollo", "subscription", Subscription);
