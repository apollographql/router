use http::HeaderName;
use http::HeaderValue;
use tower::BoxError;

use super::config::WasmGraphqlAccess;
use super::config::WasmHookConfig;
use super::config::WasmNameMatcher;
use super::config::WasmTransportAccess;
use super::wit;
use crate::Context;
use crate::graphql;
use crate::services::connector::request_service;
use crate::services::subgraph;
use crate::services::supergraph;
use apollo_federation::connectors::runtime::http_json_transport::TransportRequest;

pub(super) fn supergraph_event(
    request: &supergraph::Request,
    hook: &WasmHookConfig,
    configuration: &str,
) -> Result<wit::Event, BoxError> {
    let permissions = &hook.permissions;
    let headers = externalize_headers(
        request.supergraph_request.headers(),
        &permissions.headers.read,
    );
    let context = externalize_context(&request.context, &permissions.context.read)?;
    let body = (!matches!(permissions.graphql.request, WasmGraphqlAccess::None))
        .then(|| serde_json::to_string(request.supergraph_request.body()))
        .transpose()?;

    Ok(wit::Event {
        hook: "supergraph.request".to_string(),
        request_id: request.context.id.clone(),
        service_name: None,
        source_name: None,
        connector_name: None,
        transport: None,
        method: Some(request.supergraph_request.method().to_string()),
        uri: Some(request.supergraph_request.uri().to_string()),
        headers,
        context,
        body,
        configuration: configuration.to_string(),
    })
}

pub(super) fn subgraph_event(
    request: &subgraph::Request,
    hook: &WasmHookConfig,
    configuration: &str,
) -> Result<wit::Event, BoxError> {
    let permissions = &hook.permissions;
    let headers = externalize_headers(
        request.subgraph_request.headers(),
        &permissions.headers.read,
    );
    let context = externalize_context(&request.context, &permissions.context.read)?;
    let body = (!matches!(permissions.graphql.request, WasmGraphqlAccess::None))
        .then(|| serde_json::to_string(request.subgraph_request.body()))
        .transpose()?;

    Ok(wit::Event {
        hook: "subgraph.request".to_string(),
        request_id: request.context.id.clone(),
        service_name: Some(request.subgraph_name.clone()),
        source_name: None,
        connector_name: None,
        transport: None,
        method: Some(request.subgraph_request.method().to_string()),
        uri: Some(request.subgraph_request.uri().to_string()),
        headers,
        context,
        body,
        configuration: configuration.to_string(),
    })
}

pub(super) fn connector_event(
    request: &request_service::Request,
    source_name: Option<&str>,
    hook: &WasmHookConfig,
    configuration: &str,
) -> Result<wit::Event, BoxError> {
    let permissions = &hook.permissions;
    let (transport, method, uri, headers, body) = match &request.transport_request {
        TransportRequest::Http(http_request) => (
            "http",
            (!matches!(permissions.transport.method, WasmTransportAccess::None))
                .then(|| http_request.inner.method().to_string()),
            (!matches!(permissions.transport.uri, WasmTransportAccess::None))
                .then(|| http_request.inner.uri().to_string()),
            externalize_headers(http_request.inner.headers(), &permissions.headers.read),
            (!matches!(permissions.transport.body, WasmTransportAccess::None))
                .then(|| http_request.inner.body().clone()),
        ),
        TransportRequest::MappingOnly => ("mapping_only", None, None, Vec::new(), None),
    };

    Ok(wit::Event {
        hook: "connector.request".to_string(),
        request_id: request.context.id.clone(),
        service_name: Some(request.connector.id.subgraph_name.clone()),
        source_name: source_name.map(str::to_string),
        connector_name: Some(request.connector.id.name()),
        transport: Some(transport.to_string()),
        method,
        uri,
        headers,
        context: externalize_context(&request.context, &permissions.context.read)?,
        body,
        configuration: configuration.to_string(),
    })
}

fn externalize_headers(source: &http::HeaderMap, allowed: &WasmNameMatcher) -> Vec<wit::Header> {
    source
        .iter()
        .filter(|(name, _)| allowed.contains(name.as_str()))
        .fold(Vec::<wit::Header>::new(), |mut headers, (name, value)| {
            if let Some(existing) = headers
                .iter_mut()
                .find(|header| header.name == name.as_str())
            {
                if let Ok(value) = value.to_str() {
                    existing.values.push(value.to_string());
                }
            } else if let Ok(value) = value.to_str() {
                headers.push(wit::Header {
                    name: name.to_string(),
                    values: vec![value.to_string()],
                });
            }
            headers
        })
}

fn externalize_context(
    context: &Context,
    allowed: &WasmNameMatcher,
) -> Result<Vec<wit::ContextEntry>, BoxError> {
    context
        .iter()
        .filter(|entry| allowed.contains(entry.key()))
        .map(|entry| {
            Ok(wit::ContextEntry {
                name: entry.key().clone(),
                value: serde_json::to_string(entry.value())?,
            })
        })
        .collect()
}

pub(super) fn apply_supergraph_mutation(
    request: &mut supergraph::Request,
    hook: &WasmHookConfig,
    mutation: wit::Mutation,
) -> Result<(), BoxError> {
    if mutation.method.is_some() || mutation.uri.is_some() {
        return Err(
            "wasm plugin attempted an unsupported method or URI mutation for `supergraph.request`"
                .into(),
        );
    }
    apply_header_mutations(
        request.supergraph_request.headers_mut(),
        &hook.permissions.headers.write,
        mutation.headers,
    )?;
    apply_context_mutations(
        &request.context,
        &hook.permissions.context.write,
        mutation.context,
    )?;
    if let Some(body) = mutation.body {
        if !matches!(
            hook.permissions.graphql.request,
            WasmGraphqlAccess::ReadWrite
        ) {
            return Err(
                "wasm plugin attempted to modify the GraphQL request without write permission"
                    .into(),
            );
        }
        *request.supergraph_request.body_mut() = serde_json::from_str(&body)?;
    }
    Ok(())
}

pub(super) fn apply_subgraph_mutation(
    request: &mut subgraph::Request,
    hook: &WasmHookConfig,
    mutation: wit::Mutation,
) -> Result<(), BoxError> {
    if mutation.method.is_some() || mutation.uri.is_some() {
        return Err(
            "wasm plugin attempted an unsupported method or URI mutation for `subgraph.request`"
                .into(),
        );
    }
    apply_header_mutations(
        request.subgraph_request.headers_mut(),
        &hook.permissions.headers.write,
        mutation.headers,
    )?;
    apply_context_mutations(
        &request.context,
        &hook.permissions.context.write,
        mutation.context,
    )?;
    if let Some(body) = mutation.body {
        if !matches!(
            hook.permissions.graphql.request,
            WasmGraphqlAccess::ReadWrite
        ) {
            return Err(
                "wasm plugin attempted to modify the GraphQL request without write permission"
                    .into(),
            );
        }
        *request.subgraph_request.body_mut() = serde_json::from_str(&body)?;
    }
    Ok(())
}

pub(super) fn apply_connector_mutation(
    transport_request: &mut TransportRequest,
    context: &Context,
    hook: &WasmHookConfig,
    mutation: wit::Mutation,
) -> Result<(), BoxError> {
    if matches!(transport_request, TransportRequest::MappingOnly)
        && (!mutation.headers.is_empty()
            || mutation.method.is_some()
            || mutation.uri.is_some()
            || mutation.body.is_some())
    {
        return Err(
            "wasm plugin attempted to modify HTTP data for a mapping-only connector".into(),
        );
    }

    let context_operations =
        prepare_context_mutations(&hook.permissions.context.write, mutation.context)?;

    if let TransportRequest::Http(http_request) = transport_request {
        ensure_transport_write(
            hook.permissions.transport.method,
            mutation.method.is_some(),
            "method",
        )?;
        ensure_transport_write(
            hook.permissions.transport.uri,
            mutation.uri.is_some(),
            "URI",
        )?;
        ensure_transport_write(
            hook.permissions.transport.body,
            mutation.body.is_some(),
            "body",
        )?;

        let method = mutation
            .method
            .as_deref()
            .map(http::Method::try_from)
            .transpose()?;
        let uri = mutation
            .uri
            .as_deref()
            .map(|value| connector_uri(http_request.inner.uri(), value))
            .transpose()?;
        reject_derived_header_mutations(&mutation.headers)?;
        let mut headers = http_request.inner.headers().clone();
        apply_header_mutations(
            &mut headers,
            &hook.permissions.headers.write,
            mutation.headers,
        )?;
        // Content-Length is controlled by the transport. This also prevents a
        // header-only mutation from smuggling a length that disagrees with the body.
        headers.remove(http::header::CONTENT_LENGTH);

        *http_request.inner.headers_mut() = headers;
        if let Some(method) = method {
            *http_request.inner.method_mut() = method;
        }
        if let Some(uri) = uri {
            *http_request.inner.uri_mut() = uri;
        }
        if let Some(body) = mutation.body {
            *http_request.inner.body_mut() = body;
        }
    }
    apply_prepared_context_mutations(context, context_operations);
    Ok(())
}

fn reject_derived_header_mutations(operations: &[wit::HeaderOperation]) -> Result<(), BoxError> {
    let mut names = operations.iter().map(|operation| match operation {
        wit::HeaderOperation::Set(header) => header.name.as_str(),
        wit::HeaderOperation::Append(value) => value.name.as_str(),
        wit::HeaderOperation::Remove(name) => name.as_str(),
    });
    if names.any(|name| name.eq_ignore_ascii_case(http::header::CONTENT_LENGTH.as_str())) {
        Err("wasm plugins cannot mutate the derived `content-length` header".into())
    } else {
        Ok(())
    }
}

fn ensure_transport_write(
    access: WasmTransportAccess,
    mutated: bool,
    field: &str,
) -> Result<(), BoxError> {
    if mutated && !matches!(access, WasmTransportAccess::ReadWrite) {
        Err(format!(
            "wasm plugin attempted to modify the connector transport {field} without write permission"
        )
        .into())
    } else {
        Ok(())
    }
}

fn connector_uri(original: &http::Uri, value: &str) -> Result<http::Uri, BoxError> {
    let proposed = http::Uri::try_from(value)?;
    if proposed.scheme().is_some() || proposed.authority().is_some() {
        if proposed.scheme() != original.scheme() || proposed.authority() != original.authority() {
            return Err(
                "wasm plugin connector URI mutation cannot change scheme or authority".into(),
            );
        }
        return Ok(proposed);
    }

    let mut parts = original.clone().into_parts();
    parts.path_and_query = proposed.into_parts().path_and_query;
    Ok(http::Uri::from_parts(parts)?)
}

enum PreparedContextOperation {
    Set(String, serde_json_bytes::Value),
    Remove(String),
}

fn prepare_context_mutations(
    allowed: &WasmNameMatcher,
    operations: Vec<wit::ContextOperation>,
) -> Result<Vec<PreparedContextOperation>, BoxError> {
    operations
        .into_iter()
        .map(|operation| match operation {
            wit::ContextOperation::Set(entry) => {
                ensure_allowed(allowed, &entry.name, "context")?;
                let value: serde_json::Value = serde_json::from_str(&entry.value)?;
                Ok(PreparedContextOperation::Set(
                    entry.name,
                    serde_json_bytes::to_value(value)?,
                ))
            }
            wit::ContextOperation::Remove(name) => {
                ensure_allowed(allowed, &name, "context")?;
                Ok(PreparedContextOperation::Remove(name))
            }
        })
        .collect()
}

fn apply_prepared_context_mutations(context: &Context, operations: Vec<PreparedContextOperation>) {
    for operation in operations {
        match operation {
            PreparedContextOperation::Set(name, value) => {
                context.insert_json_value(name, value);
            }
            PreparedContextOperation::Remove(name) => {
                context.retain(|key, _| key != &name);
            }
        }
    }
}

pub(super) fn apply_header_mutations(
    headers: &mut http::HeaderMap,
    allowed: &WasmNameMatcher,
    operations: Vec<wit::HeaderOperation>,
) -> Result<(), BoxError> {
    for operation in operations {
        match operation {
            wit::HeaderOperation::Set(header) => {
                ensure_allowed(allowed, &header.name, "header")?;
                let name = HeaderName::try_from(header.name)?;
                headers.remove(&name);
                for value in header.values {
                    headers.append(name.clone(), HeaderValue::try_from(value)?);
                }
            }
            wit::HeaderOperation::Append(value) => {
                ensure_allowed(allowed, &value.name, "header")?;
                headers.append(
                    HeaderName::try_from(value.name)?,
                    HeaderValue::try_from(value.value)?,
                );
            }
            wit::HeaderOperation::Remove(name) => {
                ensure_allowed(allowed, &name, "header")?;
                headers.remove(HeaderName::try_from(name)?);
            }
        }
    }
    Ok(())
}

fn apply_context_mutations(
    context: &Context,
    allowed: &WasmNameMatcher,
    operations: Vec<wit::ContextOperation>,
) -> Result<(), BoxError> {
    for operation in operations {
        match operation {
            wit::ContextOperation::Set(entry) => {
                ensure_allowed(allowed, &entry.name, "context")?;
                let value: serde_json::Value = serde_json::from_str(&entry.value)?;
                context.insert_json_value(entry.name, serde_json_bytes::to_value(value)?);
            }
            wit::ContextOperation::Remove(name) => {
                ensure_allowed(allowed, &name, "context")?;
                context.retain(|key, _| key != &name);
            }
        }
    }
    Ok(())
}

pub(super) fn break_supergraph_response(
    context: Context,
    response: wit::BreakResponse,
) -> Result<supergraph::Response, BoxError> {
    let body: graphql::Response = serde_json::from_str(&response.body)?;
    let mut result = supergraph::Response::new_from_graphql_response(body, context);
    *result.response.status_mut() = http::StatusCode::from_u16(response.status_code)?;
    for header in response.headers {
        let name = HeaderName::try_from(header.name)?;
        for value in header.values {
            result
                .response
                .headers_mut()
                .append(name.clone(), HeaderValue::try_from(value)?);
        }
    }
    Ok(result)
}

pub(super) fn break_subgraph_response(
    request: subgraph::Request,
    response: wit::BreakResponse,
) -> Result<subgraph::Response, BoxError> {
    let body: graphql::Response = serde_json::from_str(&response.body)?;
    let mut builder = http::Response::builder().status(response.status_code);
    for header in response.headers {
        let name = HeaderName::try_from(header.name)?;
        for value in header.values {
            builder = builder.header(name.clone(), HeaderValue::try_from(value)?);
        }
    }
    Ok(subgraph::Response::new_from_response(
        builder.body(body)?,
        request.context,
        request.subgraph_name,
        request.id,
    ))
}

fn ensure_allowed(matcher: &WasmNameMatcher, name: &str, kind: &str) -> Result<(), BoxError> {
    if matcher.contains(name) {
        Ok(())
    } else {
        Err(format!("wasm plugin attempted to write unauthorized {kind} `{name}`").into())
    }
}
