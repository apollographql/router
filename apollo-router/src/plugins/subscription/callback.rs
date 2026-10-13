//! Server implementation for the callback-based subscription protocol.
use std::task::Poll;

use futures::future::BoxFuture;
use hmac::Hmac;
use hmac::Mac;
use http::HeaderName;
use http::HeaderValue;
use http::Method;
use http::StatusCode;
use once_cell::sync::OnceCell;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use tower::BoxError;
use tower::Service;
use tracing_futures::Instrument;

use crate::context::Context;
use crate::graphql;
use crate::graphql::Response;
use crate::plugins::subscription::notification::Notify;
use crate::plugins::subscription::notification::NotifyError;
use crate::services::router;
use crate::services::router::body::RouterBody;

type HmacSha256 = Hmac<sha2::Sha256>;
pub(crate) static SUBSCRIPTION_CALLBACK_HMAC_KEY: OnceCell<String> = OnceCell::new();
pub(crate) const CALLBACK_SUBSCRIPTION_HEADER_NAME: &str = "subscription-protocol";
pub(crate) const CALLBACK_SUBSCRIPTION_HEADER_VALUE: &str = "callback/1.0";

/// Parsed by [`CallbackPayload::parse`] rather than serde: internally tagged
/// enums make serde buffer the whole body through its own owned
/// representation, which the arena-backed `Response` in
/// [`SubscriptionPayload::Next`] cannot cross. `Serialize` stays for the
/// emitting side.
#[derive(Serialize, Clone, Debug)]
#[serde(tag = "kind", rename = "lowercase")]
pub(crate) enum CallbackPayload {
    #[serde(rename = "subscription")]
    Subscription(SubscriptionPayload),
}

impl CallbackPayload {
    /// Parses a callback request body.
    ///
    /// The body parses once with apollo-json and the envelope is read by
    /// key, so a `next` payload is adopted from the body's own arena rather
    /// than copied.
    fn parse(bytes: &[u8]) -> Result<CallbackPayload, String> {
        let document = apollo_json::Value::parse(bytes.to_vec()).map_err(|err| err.to_string())?;
        let root = document;
        let string_member = |name: &str| {
            root.get(name)
                .and_then(|value| value.as_string())
                .ok_or_else(|| format!("callback payload without a `{name}`"))
        };
        match string_member("kind")?.as_str() {
            "subscription" => {}
            other => return Err(format!("unknown callback payload kind `{other}`")),
        }
        let id = string_member("id")?;
        let verifier = string_member("verifier")?;

        let action = string_member("action")?;
        let payload = match action.as_str() {
            "check" => SubscriptionPayload::Check { id, verifier },
            "heartbeat" => {
                let ids = root
                    .get("ids")
                    .map(|ids| {
                        ids.array_iter()
                            .map(|id| {
                                id.as_string()
                                    .ok_or_else(|| "`ids` holds a non-string id".to_string())
                            })
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .transpose()?
                    .ok_or_else(|| "`heartbeat` without `ids`".to_string())?;
                SubscriptionPayload::Heartbeat { id, ids, verifier }
            }
            "next" => {
                let payload = root
                    .get("payload")
                    .ok_or_else(|| "`next` without a `payload`".to_string())?;
                SubscriptionPayload::Next {
                    id,
                    verifier,
                    payload: Box::new(
                        Response::from_value(payload).map_err(|err| err.to_string())?,
                    ),
                }
            }
            "complete" => {
                let errors = root
                    .get("errors")
                    .map(|errors| {
                        errors
                            .array_iter()
                            .map(|error| {
                                graphql::Error::from_value(error).map_err(|err| err.to_string())
                            })
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .transpose()?;
                SubscriptionPayload::Complete {
                    id,
                    verifier,
                    errors,
                }
            }
            other => return Err(format!("unknown callback payload action `{other}`")),
        };
        Ok(CallbackPayload::Subscription(payload))
    }

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

#[derive(Serialize, Clone, Debug)]
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
        payload: Box<Response>,
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
                        let cb_body = router::body::into_bytes(Into::<RouterBody>::into(body))
                            .await
                            .map_err(|e| format!("failed to get the request body: {e}"))
                            .and_then(|bytes| {
                                CallbackPayload::parse(&bytes).map_err(|err| {
                                    format!(
                                        "failed to deserialize the request body into JSON: {err}"
                                    )
                                })
                            });
                        let cb_body = match cb_body {
                            Ok(cb_body) => cb_body,
                            Err(err) => {
                                return router::Response::error_builder()
                                    .status_code(StatusCode::BAD_REQUEST)
                                    .error(graphql::Error::builder()
                                        .message(err)
                                        .extension_code(StatusCode::BAD_REQUEST.to_string())
                                        .build()
                                    )
                                    .context(req.context)
                                    .build();
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
                            return router::Response::error_builder()
                                .status_code(StatusCode::UNAUTHORIZED)
                                .error(graphql::Error::builder()
                                    .message("verifier doesn't match")
                                    .extension_code(StatusCode::UNAUTHORIZED.to_string())
                                    .build()
                                )
                                .context(req.context)
                                .build();
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
                                        return router::Response::error_builder()
                                            .status_code(StatusCode::NOT_FOUND)
                                            .error(graphql::Error::builder()
                                                .message("subscription doesn't exist")
                                                .extension_code(StatusCode::NOT_FOUND.to_string())
                                                .build()
                                            )
                                            .context(req.context)
                                            .build();
                                    }
                                };
                                // Keep the subscription to the client opened
                                payload.subscribed = Some(true);
                                u64_counter!(
                                    "apollo.router.operations.subscriptions.events",
                                    "Number of subscription events",
                                    1,
                                    subscriptions.mode = "callback"
                                );
                                handle.send_sync(*payload)?;

                                router::Response::builder()
                                    .context(req.context)
                                    .build()
                            }
                            CallbackPayload::Subscription(SubscriptionPayload::Check {
                                ..
                            }) => {
                                if notify.exist(id).await? {
                                    router::Response::error_builder()
                                        .status_code(StatusCode::NO_CONTENT)
                                        .header(HeaderName::from_static(CALLBACK_SUBSCRIPTION_HEADER_NAME), HeaderValue::from_static(CALLBACK_SUBSCRIPTION_HEADER_VALUE))
                                        .error(graphql::Error::builder()
                                            .message(String::default())
                                            .extension_code(StatusCode::NO_CONTENT.to_string())
                                            .build()
                                        )
                                        .context(req.context)
                                        .build()
                                } else {
                                    router::Response::error_builder()
                                        .status_code(StatusCode::NOT_FOUND)
                                        .header(HeaderName::from_static(CALLBACK_SUBSCRIPTION_HEADER_NAME), HeaderValue::from_static(CALLBACK_SUBSCRIPTION_HEADER_VALUE))
                                        .error(graphql::Error::builder()
                                            .message("subscription doesn't exist")
                                            .extension_code(StatusCode::NOT_FOUND.to_string())
                                            .build()
                                        )
                                        .context(req.context)
                                        .build()
                                }
                            }
                            CallbackPayload::Subscription(SubscriptionPayload::Heartbeat {
                                ids,
                                id,
                                verifier,
                            }) => {
                                if !ids.contains(&id) {
                                    return router::Response::error_builder()
                                        .status_code(StatusCode::UNAUTHORIZED)
                                        .error(graphql::Error::builder()
                                            .message("id used for the verifier is not part of ids array")
                                            .extension_code(StatusCode::UNAUTHORIZED.to_string())
                                            .build()
                                        )
                                        .context(req.context)
                                        .build()
                                }

                                let (mut valid_ids, invalid_ids) = notify.invalid_ids(ids).await?;
                                if invalid_ids.is_empty() {
                                    router::Response::error_builder()
                                        .status_code(StatusCode::NO_CONTENT)
                                        .error(graphql::Error::builder()
                                            .message(String::default())
                                            .extension_code(StatusCode::NO_CONTENT.to_string())
                                            .build()
                                        )
                                        .context(req.context)
                                        .build()
                                } else if valid_ids.is_empty() {
                                    router::Response::error_builder()
                                        .status_code(StatusCode::NOT_FOUND)
                                        .error(graphql::Error::builder()
                                            .message("subscriptions don't exist")
                                            .extension_code(StatusCode::NOT_FOUND.to_string())
                                            .build()
                                        )
                                        .context(req.context)
                                        .build()
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
                                    router::Response::error_builder()
                                        .status_code(StatusCode::NOT_FOUND)
                                        .error(graphql::Error::builder()
                                            .message(serde_json::to_string_pretty(&InvalidIdsPayload{
                                                invalid_ids,
                                                id,
                                                verifier,
                                            }).map_err(BoxError::from)?)
                                            .extension_code(StatusCode::NOT_FOUND.to_string())
                                            .build()
                                        )
                                        .context(req.context)
                                        .build()
                                }
                            }
                            CallbackPayload::Subscription(SubscriptionPayload::Complete {
                                errors,
                                ..
                            }) => {
                                if let Some(errors) = errors {
                                    let mut handle = match notify.subscribe(id.clone()).await {
                                         Ok(handle) => handle.into_sink(),
                                         Err(NotifyError::UnknownTopic) => {
                                            return router::Response::error_builder()
                                                .status_code(StatusCode::NOT_FOUND)
                                                .error(graphql::Error::builder()
                                                    .message("unknown topic")
                                                    .extension_code(StatusCode::NOT_FOUND.to_string())
                                                    .build()
                                                )
                                                .context(req.context)
                                                .build();
                                         },
                                         Err(err) => {
                                            return router::Response::error_builder()
                                                .status_code(StatusCode::NOT_FOUND)
                                                .error(graphql::Error::builder()
                                                    .message(err.to_string())
                                                    .extension_code(StatusCode::NOT_FOUND.to_string())
                                                    .build()
                                                )
                                                .context(req.context)
                                                .build();
                                         }
                                    };
                                    u64_counter!(
                                        "apollo.router.operations.subscriptions.events",
                                        "Number of subscription events",
                                        1,
                                        subscriptions.mode = "callback",
                                        subscriptions.complete = true
                                    );
                                    if let Err(_err) = handle.send_sync(
                                        graphql::Response::builder().errors(errors).build(),
                                    ) {
                                       return router::Response::error_builder()
                                            .status_code(StatusCode::NOT_FOUND)
                                            .error(graphql::Error::builder()
                                                .message("cannot send errors to the client")
                                                .extension_code(StatusCode::NOT_FOUND.to_string())
                                                .build()
                                            )
                                            .context(req.context)
                                            .build();
                                    }
                                }
                                if let Err(_err) = notify.force_delete(id).await {
                                    return router::Response::error_builder()
                                        .status_code(StatusCode::NOT_FOUND)
                                        .error(graphql::Error::builder()
                                            .message("cannot force delete")
                                            .extension_code(StatusCode::NOT_FOUND.to_string())
                                            .build()
                                        )
                                        .context(req.context)
                                        .build();
                                }

                                router::Response::http_response_builder()
                                    .response(http::Response::builder()
                                        .status(StatusCode::ACCEPTED)
                                        .body(router::body::empty())
                                        .map_err(BoxError::from)?)
                                    .context(req.context)
                                    .build()
                            }
                        }
                    }
                    _ => router::Response::error_builder()
                        .status_code(StatusCode::METHOD_NOT_ALLOWED)
                        .error(graphql::Error::builder()
                            .message(String::default())
                            .extension_code(StatusCode::METHOD_NOT_ALLOWED.to_string())
                            .build()
                        )
                        .context(req.context)
                        .build()
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

#[allow(clippy::result_large_err)]
fn ensure_id_consistency(
    context: &Context,
    id_from_path: &str,
    id_from_body: &str,
) -> Result<(), router::Response> {
    if id_from_path != id_from_body {
        Err(router::Response::error_builder()
            .status_code(StatusCode::BAD_REQUEST)
            .error(
                graphql::Error::builder()
                    .message("id from url path and id from body are different")
                    .extension_code(StatusCode::BAD_REQUEST.to_string())
                    .build(),
            )
            .context(context.clone())
            .build()
            .expect("this response is valid"))
    } else {
        Ok(())
    }
}
