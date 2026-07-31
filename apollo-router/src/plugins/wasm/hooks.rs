use http::HeaderName;
use http::HeaderValue;
use tower::BoxError;

use super::config::WasmGraphqlAccess;
use super::config::WasmHookConfig;
use super::config::WasmNameMatcher;
use super::wit;
use crate::Context;
use crate::graphql;
use crate::services::subgraph;
use crate::services::supergraph;

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
        method: Some(request.subgraph_request.method().to_string()),
        uri: Some(request.subgraph_request.uri().to_string()),
        headers,
        context,
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
