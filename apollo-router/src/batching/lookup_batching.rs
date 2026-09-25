//! Transport batching of GraphQL Federation lookup requests.
//!
//! A lookup fetch sends one subgraph request per entity. Each goes through the subgraph pipeline
//! (plugins, coprocessors, telemetry) on its own; when the subgraph is configured for batching, the
//! requests of a batch carry a [`LookupBatchSlot`] in their HTTP extensions, and
//! [`JoinLookupBatchesLayer`], at the top of the HTTP client stack, joins them into batched HTTP
//! requests following the draft "Batching" appendix of the GraphQL-over-HTTP specification:
//!
//! - **variable batching**: one request object with a list of variable sets,
//!   `{"query": ..., "variables": [{...}, {...}]}`;
//! - **request batching**: a list of request objects, `[{"query": ...}, {"query": ...}]`;
//! - both together: a list of request objects, each with a list of variable sets.
//!
//! Responses are read as `application/jsonl` (one result per line, placed by `requestIndex` and
//! `variableIndex`, in any order). A JSON array answer to request batching is also accepted and
//! read positionally, which is how servers implementing plain array batching respond.
//!
//! A batch is sent once every member has either reached the layer or been dropped (e.g. a plugin
//! answered the request itself), so a request that never reaches the network cannot hold the
//! others back.

use std::sync::Arc;

use bytes::Bytes;
use futures::future::BoxFuture;
use http::HeaderValue;
use http::StatusCode;
use http::header::ACCEPT;
use http::header::CONTENT_LENGTH;
use http::header::CONTENT_TYPE;
use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::oneshot;
use tower::BoxError;
use tower::ServiceExt as _;

use crate::Context;
use crate::services::http::HttpRequest;
use crate::services::http::HttpResponse;
use crate::services::router::body::RouterBody;

const JSONL: &str = "application/jsonl";

/// How a batch is sent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct LookupBatchMode {
    pub(crate) variable_batching: bool,
    pub(crate) request_batching: bool,
    /// Maximum number of operations (variable sets) per HTTP request.
    pub(crate) maximum_size: Option<usize>,
}

/// A member's request as it reached the layer.
struct Member {
    parts: http::request::Parts,
    body: serde_json::Map<String, Value>,
    context: Context,
    sender: oneshot::Sender<Result<HttpResponse, BoxError>>,
}

enum MemberState {
    Pending,
    Submitted(Box<Member>),
    Dropped,
}

struct State {
    /// Fetches that may still join the batch.
    expected_fetches: usize,
    /// Members of each joined fetch.
    fetches: Vec<Vec<MemberState>>,
    /// Members neither submitted nor dropped yet.
    outstanding: usize,
    /// The HTTP client the batch is sent through (from the first submission).
    service: Option<crate::services::http::BoxCloneService>,
    sent: bool,
}

/// Coordinates the requests of one batch.
pub(crate) struct LookupBatch {
    mode: LookupBatchMode,
    state: Mutex<State>,
}

impl LookupBatch {
    /// A batch that `expected_fetches` fetches will join (or release).
    pub(crate) fn new(mode: LookupBatchMode, expected_fetches: usize) -> Arc<Self> {
        Arc::new(Self {
            mode,
            state: Mutex::new(State {
                expected_fetches,
                fetches: Vec::new(),
                outstanding: 0,
                service: None,
                sent: false,
            }),
        })
    }

    /// Join the batch with a fetch of `members` requests; returns one slot per request.
    pub(crate) fn join(self: &Arc<Self>, members: usize) -> Vec<LookupBatchSlot> {
        let mut state = self.state.lock();
        state.expected_fetches = state.expected_fetches.saturating_sub(1);
        let fetch = state.fetches.len();
        state
            .fetches
            .push((0..members).map(|_| MemberState::Pending).collect());
        state.outstanding += members;
        let ready = self.take_if_ready(&mut state);
        drop(state);
        if let Some(ready) = ready {
            self.send(ready);
        }
        (0..members)
            .map(|member| LookupBatchSlot {
                batch: self.clone(),
                fetch,
                member,
                consumed: false,
            })
            .collect()
    }

    /// A fetch that was expected to join will not (it had nothing to fetch, or was skipped).
    pub(crate) fn release(self: &Arc<Self>) {
        let mut state = self.state.lock();
        state.expected_fetches = state.expected_fetches.saturating_sub(1);
        let ready = self.take_if_ready(&mut state);
        drop(state);
        if let Some(ready) = ready {
            self.send(ready);
        }
    }

    fn submit(
        self: &Arc<Self>,
        fetch: usize,
        member: usize,
        submitted: Member,
        service: crate::services::http::BoxCloneService,
    ) {
        let mut state = self.state.lock();
        if state.service.is_none() {
            state.service = Some(service);
        }
        if let Some(slot) = state.fetches.get_mut(fetch).and_then(|f| f.get_mut(member)) {
            *slot = MemberState::Submitted(Box::new(submitted));
            state.outstanding = state.outstanding.saturating_sub(1);
        }
        let ready = self.take_if_ready(&mut state);
        drop(state);
        if let Some(ready) = ready {
            self.send(ready);
        }
    }

    fn drop_member(self: &Arc<Self>, fetch: usize, member: usize) {
        let mut state = self.state.lock();
        if let Some(slot) = state.fetches.get_mut(fetch).and_then(|f| f.get_mut(member))
            && matches!(slot, MemberState::Pending)
        {
            *slot = MemberState::Dropped;
            state.outstanding = state.outstanding.saturating_sub(1);
        }
        let ready = self.take_if_ready(&mut state);
        drop(state);
        if let Some(ready) = ready {
            self.send(ready);
        }
    }

    /// When every expected fetch joined and every member arrived or was dropped, take what to
    /// send.
    #[allow(clippy::type_complexity)]
    fn take_if_ready(
        &self,
        state: &mut State,
    ) -> Option<(crate::services::http::BoxCloneService, Vec<Vec<Member>>)> {
        if state.sent || state.expected_fetches > 0 || state.outstanding > 0 {
            return None;
        }
        state.sent = true;
        let service = state.service.take()?;
        let fetches = std::mem::take(&mut state.fetches)
            .into_iter()
            .map(|members| {
                members
                    .into_iter()
                    .filter_map(|m| match m {
                        MemberState::Submitted(member) => Some(*member),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|members| !members.is_empty())
            .collect();
        Some((service, fetches))
    }

    fn send(&self, (service, fetches): (crate::services::http::BoxCloneService, Vec<Vec<Member>>)) {
        let mode = self.mode;
        tokio::spawn(async move {
            for batch in plan_http_requests(mode, fetches) {
                send_http_request(service.clone(), mode, batch).await;
            }
        });
    }
}

/// A fetch's place in a batch shared with other fetches running at the same time (request
/// batching across fetches). Joining uses it; dropping it unused (the fetch had nothing to fetch,
/// or ended early) releases the batch so the others are not held back.
pub(crate) struct LookupBatchTicket {
    batch: Arc<LookupBatch>,
    used: bool,
}

impl LookupBatchTicket {
    pub(crate) fn new(batch: Arc<LookupBatch>) -> Self {
        Self { batch, used: false }
    }

    pub(crate) fn join(mut self, members: usize) -> Vec<LookupBatchSlot> {
        self.used = true;
        self.batch.join(members)
    }
}

impl Drop for LookupBatchTicket {
    fn drop(&mut self) {
        if !self.used {
            self.batch.release();
        }
    }
}

/// Tickets of the fetches of the current `Parallel` nodes, by fetch node address. Kept in the
/// request context.
#[derive(Default)]
pub(crate) struct LookupBatchTickets(
    pub(crate) Mutex<std::collections::HashMap<usize, LookupBatchTicket>>,
);

impl LookupBatchTickets {
    pub(crate) fn take(&self, fetch_node_address: usize) -> Option<LookupBatchTicket> {
        self.0.lock().remove(&fetch_node_address)
    }
}

/// A request's place in a batch. Carried in the request's HTTP extensions; dropping it without
/// submitting (the request never reached the network) releases its place.
pub(crate) struct LookupBatchSlot {
    batch: Arc<LookupBatch>,
    fetch: usize,
    member: usize,
    consumed: bool,
}

impl Clone for LookupBatchSlot {
    // HTTP extensions require `Clone`. A clone does not hold the place: only the original
    // releases it.
    fn clone(&self) -> Self {
        Self {
            batch: self.batch.clone(),
            fetch: self.fetch,
            member: self.member,
            consumed: true,
        }
    }
}

impl Drop for LookupBatchSlot {
    fn drop(&mut self) {
        if !self.consumed {
            self.batch.drop_member(self.fetch, self.member);
        }
    }
}

/// One request object of a batched HTTP request: its members, by variable index.
struct RequestObject {
    members: Vec<Member>,
    /// Whether its variables are sent as a list (variable batching).
    variable_list: bool,
}

/// One HTTP request of a batch.
struct HttpBatch {
    objects: Vec<RequestObject>,
    /// Whether the body is a list of request objects (request batching).
    request_list: bool,
}

fn plan_http_requests(mode: LookupBatchMode, mut fetches: Vec<Vec<Member>>) -> Vec<HttpBatch> {
    let maximum = mode.maximum_size.filter(|m| *m > 0).unwrap_or(usize::MAX);
    // Fetches join in whatever order they run; order them by operation so that the batch does
    // not depend on scheduling.
    fetches.sort_by(|a, b| {
        let query = |members: &Vec<Member>| {
            members
                .first()
                .and_then(|m| m.body.get("query"))
                .and_then(|q| q.as_str())
                .map(str::to_owned)
                .unwrap_or_default()
        };
        query(a).cmp(&query(b))
    });
    // Request objects: per fetch, one with all variable sets (variable batching), or one per
    // member.
    let mut objects: Vec<RequestObject> = Vec::new();
    for members in fetches {
        if mode.variable_batching {
            let mut members = members;
            while !members.is_empty() {
                let rest = members.split_off(members.len().min(maximum));
                objects.push(RequestObject {
                    members,
                    variable_list: true,
                });
                members = rest;
            }
        } else {
            objects.extend(members.into_iter().map(|member| RequestObject {
                members: vec![member],
                variable_list: false,
            }));
        }
    }
    if !mode.request_batching {
        return objects
            .into_iter()
            .map(|object| HttpBatch {
                objects: vec![object],
                request_list: false,
            })
            .collect();
    }
    // Pack request objects into HTTP requests of at most `maximum` operations.
    let mut batches: Vec<HttpBatch> = Vec::new();
    let mut current: Vec<RequestObject> = Vec::new();
    let mut size = 0;
    for object in objects {
        let object_size = object.members.len();
        if !current.is_empty() && size + object_size > maximum {
            batches.push(HttpBatch {
                objects: std::mem::take(&mut current),
                request_list: true,
            });
            size = 0;
        }
        size += object_size;
        current.push(object);
    }
    if !current.is_empty() {
        batches.push(HttpBatch {
            objects: current,
            request_list: true,
        });
    }
    batches
}

fn request_object_json(object: &RequestObject) -> Value {
    let first = &object.members[0].body;
    let mut json = serde_json::Map::new();
    for key in ["query", "operationName", "extensions"] {
        if let Some(value) = first.get(key)
            && !value.is_null()
        {
            json.insert(key.to_string(), value.clone());
        }
    }
    let variables = |member: &Member| {
        member
            .body
            .get("variables")
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default()))
    };
    json.insert(
        "variables".to_string(),
        if object.variable_list {
            Value::Array(object.members.iter().map(variables).collect())
        } else {
            variables(&object.members[0])
        },
    );
    Value::Object(json)
}

fn respond(member: Member, status: StatusCode, headers: &http::HeaderMap, body: Value) {
    let mut response = http::Response::builder().status(status);
    if let Some(response_headers) = response.headers_mut() {
        for (name, value) in headers {
            if name != CONTENT_TYPE && name != CONTENT_LENGTH {
                response_headers.append(name, value.clone());
            }
        }
        response_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    }
    let body =
        crate::services::router::body::from_bytes(serde_json::to_vec(&body).unwrap_or_default());
    let result = response
        .body(body)
        .map(|http_response| HttpResponse {
            http_response,
            context: member.context.clone(),
        })
        .map_err(BoxError::from);
    let _ = member.sender.send(result);
}

fn fail(member: Member, error: &str) {
    let _ = member.sender.send(Err(BoxError::from(error.to_string())));
}

async fn send_http_request(
    service: crate::services::http::BoxCloneService,
    mode: LookupBatchMode,
    batch: HttpBatch,
) {
    let HttpBatch {
        mut objects,
        request_list,
    } = batch;
    let body = if request_list {
        Value::Array(objects.iter().map(request_object_json).collect())
    } else {
        request_object_json(&objects[0])
    };
    let first = &objects[0].members[0];
    let mut parts = first.parts.clone();
    parts.extensions = http::Extensions::new();
    parts.headers.remove(CONTENT_LENGTH);
    parts.headers.insert(
        ACCEPT,
        HeaderValue::from_static(
            "application/jsonl, application/graphql-response+json;q=0.9, application/json;q=0.8",
        ),
    );
    parts
        .headers
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let context = first.context.clone();
    let request = http::Request::from_parts(
        parts,
        crate::services::router::body::from_bytes(serde_json::to_vec(&body).unwrap_or_default()),
    );
    u64_counter!(
        "apollo.router.operations.graphql_federation.lookup_batches",
        "Number of batched lookup requests sent to subgraphs",
        1,
        variable_batching = mode.variable_batching,
        request_batching = mode.request_batching
    );

    let response = match service
        .oneshot(HttpRequest {
            http_request: request,
            context,
        })
        .await
    {
        Ok(response) => response,
        Err(error) => {
            let message = format!("batched lookup request failed: {error}");
            for object in objects {
                for member in object.members {
                    fail(member, &message);
                }
            }
            return;
        }
    };
    let (parts, response_body) = response.http_response.into_parts();
    let bytes: Bytes = match crate::services::router::body::into_bytes(response_body).await {
        Ok(bytes) => bytes,
        Err(error) => {
            let message = format!("could not read the batched lookup response: {error}");
            for object in objects {
                for member in object.members {
                    fail(member, &message);
                }
            }
            return;
        }
    };

    // Not a batch response: hand every member the same answer, so each reports it.
    if !parts.status.is_success() {
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        for object in objects {
            for member in object.members {
                respond(member, parts.status, &parts.headers, body.clone());
            }
        }
        return;
    }

    // Placed results, by (request object, variable index).
    let mut results: Vec<Vec<Option<Value>>> = objects
        .iter()
        .map(|o| vec![None; o.members.len()])
        .collect();
    let content_type = parts
        .headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut place = |request_index: usize, variable_index: usize, mut entry: Value| {
        if let Value::Object(object) = &mut entry {
            object.remove("requestIndex");
            object.remove("variableIndex");
        }
        if let Some(slot) = results
            .get_mut(request_index)
            .and_then(|r| r.get_mut(variable_index))
        {
            *slot = Some(entry);
        }
    };
    let index = |entry: &Value, key: &str| {
        entry
            .get(key)
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(0)
    };
    if content_type.contains("jsonl") || content_type.contains("ndjson") {
        for line in bytes.split(|b| *b == b'\n') {
            let line = line.trim_ascii();
            if line.is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_slice::<Value>(line) {
                let request_index = index(&entry, "requestIndex");
                let variable_index = index(&entry, "variableIndex");
                place(request_index, variable_index, entry);
            }
        }
    } else {
        match serde_json::from_slice::<Value>(&bytes) {
            // Plain array batching: one result per request object, in order.
            Ok(Value::Array(entries)) if request_list => {
                for (request_index, entry) in entries.into_iter().enumerate() {
                    place(request_index, 0, entry);
                }
            }
            // A single result for a single operation.
            Ok(entry @ Value::Object(_)) if objects.len() == 1 && objects[0].members.len() == 1 => {
                place(0, 0, entry);
            }
            _ => {}
        }
    }

    for (object, object_results) in objects.drain(..).zip(results) {
        for (member, result) in object.members.into_iter().zip(object_results) {
            match result {
                Some(result) => respond(member, parts.status, &parts.headers, result),
                None => respond(
                    member,
                    parts.status,
                    &parts.headers,
                    serde_json::json!({
                        "errors": [{
                            "message": format!(
                                "the subgraph's batched response has no result for this operation \
                                 (response content type: {content_type:?}); check that the \
                                 subgraph supports GraphQL-over-HTTP batching ({JSONL})"
                            ),
                        }],
                    }),
                ),
            }
        }
    }
}

/// Joins requests carrying a [`LookupBatchSlot`] into batched HTTP requests (see the module
/// documentation); other requests pass through.
#[derive(Clone, Default)]
pub(crate) struct JoinLookupBatchesLayer;

impl<S> tower::Layer<S> for JoinLookupBatchesLayer {
    type Service = JoinLookupBatchesService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        JoinLookupBatchesService { inner }
    }
}

#[derive(Clone)]
pub(crate) struct JoinLookupBatchesService<S> {
    inner: S,
}

impl<S> tower::Service<HttpRequest> for JoinLookupBatchesService<S>
where
    S: tower::Service<HttpRequest, Response = HttpResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = HttpResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<HttpResponse, BoxError>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut request: HttpRequest) -> Self::Future {
        let Some(mut slot) = request
            .http_request
            .extensions_mut()
            .remove::<LookupBatchSlot>()
        else {
            let fresh = self.inner.clone();
            let inner = std::mem::replace(&mut self.inner, fresh);
            return Box::pin(inner.oneshot(request));
        };
        let service = crate::services::http::BoxCloneService::new(self.inner.clone());
        Box::pin(async move {
            let HttpRequest {
                http_request,
                context,
            } = request;
            let (parts, body) = http_request.into_parts();
            let body: RouterBody = body;
            let bytes = crate::services::router::body::into_bytes(body)
                .await
                .map_err(BoxError::from)?;
            let body = match serde_json::from_slice::<Value>(&bytes) {
                Ok(Value::Object(body)) => body,
                _ => return Err(BoxError::from("lookup request body is not a JSON object")),
            };
            let (sender, receiver) = oneshot::channel();
            slot.consumed = true;
            slot.batch.clone().submit(
                slot.fetch,
                slot.member,
                Member {
                    parts,
                    body,
                    context,
                    sender,
                },
                service,
            );
            receiver
                .await
                .map_err(|_| BoxError::from("the lookup batch was dropped"))?
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use parking_lot::Mutex;
    use serde_json::json;
    use tower::Service as _;

    use super::*;

    /// A fake subgraph: records each HTTP request body and answers with `respond(body)`.
    fn fake_subgraph(
        seen: Arc<Mutex<Vec<Value>>>,
        content_type: &'static str,
        respond: fn(&Value) -> String,
    ) -> crate::services::http::BoxCloneService {
        crate::services::http::BoxCloneService::new(tower::service_fn(
            move |request: HttpRequest| {
                let seen = seen.clone();
                async move {
                    let bytes =
                        crate::services::router::body::into_bytes(request.http_request.into_body())
                            .await?;
                    let body: Value = serde_json::from_slice(&bytes)?;
                    let response = respond(&body);
                    seen.lock().push(body);
                    Ok::<_, BoxError>(HttpResponse {
                        http_response: http::Response::builder()
                            .status(200)
                            .header(CONTENT_TYPE, content_type)
                            .body(crate::services::router::body::from_bytes(response))
                            .unwrap(),
                        context: request.context,
                    })
                }
            },
        ))
    }

    fn member_request(slot: LookupBatchSlot, query: &str, id: &str) -> HttpRequest {
        let mut request = http::Request::builder()
            .method("POST")
            .uri("http://subgraph/graphql")
            .body(crate::services::router::body::from_bytes(
                serde_json::to_vec(&json!({
                    "query": query,
                    "variables": {"lookupArgument_0": id},
                }))
                .unwrap(),
            ))
            .unwrap();
        request.extensions_mut().insert(slot);
        HttpRequest {
            http_request: request,
            context: Context::new(),
        }
    }

    async fn body_of(response: HttpResponse) -> Value {
        let bytes = crate::services::router::body::into_bytes(response.http_response.into_body())
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    const QUERY: &str =
        "query($lookupArgument_0: ID!) { productById(id: $lookupArgument_0) { name } }";

    #[tokio::test]
    async fn variable_batching_sends_one_request_and_places_results_by_index() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let service = fake_subgraph(seen.clone(), "application/jsonl", |_| {
            // Out of order, as the draft allows.
            [
                r#"{"variableIndex": 2, "data": {"productById": {"name": "c"}}}"#,
                r#"{"variableIndex": 0, "data": {"productById": {"name": "a"}}}"#,
                r#"{"variableIndex": 1, "data": {"productById": null}, "errors": [{"message": "gone"}]}"#,
            ]
            .join("\n")
        });
        let layer = JoinLookupBatchesLayer;
        let batch = LookupBatch::new(
            LookupBatchMode {
                variable_batching: true,
                ..Default::default()
            },
            1,
        );
        let slots = batch.join(3);
        let calls = slots.into_iter().zip(["1", "2", "3"]).map(|(slot, id)| {
            let mut service = tower::Layer::layer(&layer, service.clone());
            async move {
                let request = member_request(slot, QUERY, id);
                service.ready().await.unwrap().call(request).await.unwrap()
            }
        });
        let responses = futures::future::join_all(calls).await;
        let mut bodies = Vec::new();
        for response in responses {
            bodies.push(body_of(response).await);
        }
        assert_eq!(bodies[0], json!({"data": {"productById": {"name": "a"}}}));
        assert_eq!(
            bodies[1],
            json!({"data": {"productById": null}, "errors": [{"message": "gone"}]})
        );
        assert_eq!(bodies[2], json!({"data": {"productById": {"name": "c"}}}));
        assert_eq!(
            *seen.lock(),
            vec![json!({
                "query": QUERY,
                "variables": [
                    {"lookupArgument_0": "1"},
                    {"lookupArgument_0": "2"},
                    {"lookupArgument_0": "3"},
                ],
            })]
        );
    }

    #[tokio::test]
    async fn request_batching_accepts_array_responses() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let service = fake_subgraph(seen.clone(), "application/json", |_| {
            r#"[{"data": {"productById": {"name": "a"}}}, {"data": {"productById": {"name": "b"}}}]"#
                .to_string()
        });
        let layer = JoinLookupBatchesLayer;
        let batch = LookupBatch::new(
            LookupBatchMode {
                request_batching: true,
                ..Default::default()
            },
            1,
        );
        let slots = batch.join(2);
        let calls = slots.into_iter().zip(["1", "2"]).map(|(slot, id)| {
            let mut service = tower::Layer::layer(&layer, service.clone());
            async move {
                let request = member_request(slot, QUERY, id);
                service.ready().await.unwrap().call(request).await.unwrap()
            }
        });
        let responses = futures::future::join_all(calls).await;
        assert_eq!(
            body_of(responses.into_iter().nth(1).unwrap()).await,
            json!({"data": {"productById": {"name": "b"}}})
        );
        assert_eq!(
            *seen.lock(),
            vec![json!([
                {"query": QUERY, "variables": {"lookupArgument_0": "1"}},
                {"query": QUERY, "variables": {"lookupArgument_0": "2"}},
            ])]
        );
    }

    #[tokio::test]
    async fn combined_batching_across_fetches_and_maximum_size() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let service = fake_subgraph(seen.clone(), "application/jsonl", |body| {
            // Answer every operation of every request object.
            let mut lines = Vec::new();
            for (request_index, object) in body.as_array().unwrap().iter().enumerate() {
                for (variable_index, variables) in
                    object["variables"].as_array().unwrap().iter().enumerate()
                {
                    lines.push(
                        json!({
                            "requestIndex": request_index,
                            "variableIndex": variable_index,
                            "data": {"echo": variables["lookupArgument_0"]},
                        })
                        .to_string(),
                    );
                }
            }
            lines.join("\n")
        });
        let layer = JoinLookupBatchesLayer;
        let batch = LookupBatch::new(
            LookupBatchMode {
                variable_batching: true,
                request_batching: true,
                maximum_size: Some(3),
            },
            2,
        );
        let first = batch.join(2);
        let second = batch.join(2);
        let mut calls = Vec::new();
        for (slot, (query, id)) in first.into_iter().chain(second).zip([
            ("{ a }", "1"),
            ("{ a }", "2"),
            ("{ b }", "3"),
            ("{ b }", "4"),
        ]) {
            let mut service = tower::Layer::layer(&layer, service.clone());
            calls.push(async move {
                let request = member_request(slot, query, id);
                service.ready().await.unwrap().call(request).await.unwrap()
            });
        }
        let responses = futures::future::join_all(calls).await;
        let mut echoed = Vec::new();
        for response in responses {
            echoed.push(body_of(response).await["data"]["echo"].clone());
        }
        assert_eq!(echoed, [json!("1"), json!("2"), json!("3"), json!("4")]);
        // 4 operations with a maximum of 3 per HTTP request: two requests.
        assert_eq!(seen.lock().len(), 2);
    }

    #[tokio::test]
    async fn a_member_dropped_before_the_network_does_not_block_the_batch() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let service = fake_subgraph(seen.clone(), "application/jsonl", |_| {
            r#"{"variableIndex": 0, "data": {"productById": {"name": "a"}}}"#.to_string()
        });
        let layer = JoinLookupBatchesLayer;
        let batch = LookupBatch::new(
            LookupBatchMode {
                variable_batching: true,
                ..Default::default()
            },
            1,
        );
        let mut slots = batch.join(2);
        // A plugin answered the second request itself: its slot is dropped.
        drop(slots.pop());
        let mut service = tower::Layer::layer(&layer, service);
        let response = service
            .ready()
            .await
            .unwrap()
            .call(member_request(slots.pop().unwrap(), QUERY, "1"))
            .await
            .unwrap();
        assert_eq!(
            body_of(response).await,
            json!({"data": {"productById": {"name": "a"}}})
        );
        assert_eq!(seen.lock().len(), 1);
    }

    #[tokio::test]
    async fn requests_without_a_slot_pass_through() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let service = fake_subgraph(seen.clone(), "application/json", |_| {
            r#"{"data": {"a": 1}}"#.to_string()
        });
        let mut service = tower::Layer::layer(&JoinLookupBatchesLayer, service);
        let request = HttpRequest {
            http_request: http::Request::builder()
                .method("POST")
                .uri("http://subgraph/graphql")
                .body(crate::services::router::body::from_bytes(
                    r#"{"query":"{ a }"}"#,
                ))
                .unwrap(),
            context: Context::new(),
        };
        let response = service.ready().await.unwrap().call(request).await.unwrap();
        assert_eq!(body_of(response).await, json!({"data": {"a": 1}}));
        assert_eq!(*seen.lock(), vec![json!({"query": "{ a }"})]);
    }
}
