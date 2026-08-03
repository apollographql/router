use tower::BoxError;

use super::super::config::WasmGraphqlAccess;
use super::super::config::WasmHookConfig;
use super::super::wit;
use super::mutation;
use crate::Context;
use crate::graphql;
use crate::services::subgraph;
use crate::services::supergraph;

pub(in crate::plugins::wasm) fn supergraph_event(
    request: &supergraph::Request,
    hook: &WasmHookConfig,
    configuration: &str,
) -> Result<wit::Event, BoxError> {
    graphql_event(
        "supergraph.request",
        None,
        &request.context,
        &request.supergraph_request,
        hook,
        configuration,
    )
}

pub(in crate::plugins::wasm) fn subgraph_event(
    request: &subgraph::Request,
    hook: &WasmHookConfig,
    configuration: &str,
) -> Result<wit::Event, BoxError> {
    graphql_event(
        "subgraph.request",
        Some(&request.subgraph_name),
        &request.context,
        &request.subgraph_request,
        hook,
        configuration,
    )
}

fn graphql_event(
    hook_name: &str,
    service_name: Option<&str>,
    context: &Context,
    request: &http::Request<graphql::Request>,
    hook: &WasmHookConfig,
    configuration: &str,
) -> Result<wit::Event, BoxError> {
    let permissions = &hook.permissions;
    let body = (!matches!(permissions.graphql.request, WasmGraphqlAccess::None))
        .then(|| serde_json::to_string(request.body()))
        .transpose()?;

    Ok(wit::Event {
        hook: hook_name.to_string(),
        request_id: context.id.clone(),
        service_name: service_name.map(str::to_string),
        source_name: None,
        connector_name: None,
        transport: None,
        method: Some(request.method().to_string()),
        uri: Some(request.uri().to_string()),
        headers: mutation::externalize_headers(request.headers(), &permissions.headers.read),
        context: mutation::externalize_context(context, &permissions.context.read)?,
        body,
        configuration: configuration.to_string(),
    })
}

pub(in crate::plugins::wasm) fn apply_supergraph_mutation(
    request: &mut supergraph::Request,
    hook: &WasmHookConfig,
    mutation: wit::Mutation,
) -> Result<(), BoxError> {
    apply_graphql_mutation(
        "supergraph.request",
        &mut request.supergraph_request,
        &request.context,
        hook,
        mutation,
    )
}

pub(in crate::plugins::wasm) fn apply_subgraph_mutation(
    request: &mut subgraph::Request,
    hook: &WasmHookConfig,
    mutation: wit::Mutation,
) -> Result<(), BoxError> {
    apply_graphql_mutation(
        "subgraph.request",
        &mut request.subgraph_request,
        &request.context,
        hook,
        mutation,
    )
}

fn apply_graphql_mutation(
    hook_name: &str,
    request: &mut http::Request<graphql::Request>,
    context: &Context,
    hook: &WasmHookConfig,
    mutation: wit::Mutation,
) -> Result<(), BoxError> {
    let wit::Mutation {
        headers: header_operations,
        context: context_operations,
        method,
        uri,
        body: body_mutation,
    } = mutation;
    if method.is_some() || uri.is_some() {
        return Err(format!(
            "wasm plugin attempted an unsupported method or URI mutation for `{hook_name}`"
        )
        .into());
    }

    let prepared_headers = mutation::prepare_header_mutations(
        request.headers(),
        &hook.permissions.headers.write,
        header_operations,
    )?;
    let prepared_context =
        mutation::prepare_context_mutations(&hook.permissions.context.write, context_operations)?;
    let prepared_body = body_mutation
        .map(|body| {
            if !matches!(
                hook.permissions.graphql.request,
                WasmGraphqlAccess::ReadWrite
            ) {
                return Err::<graphql::Request, BoxError>(
                    "wasm plugin attempted to modify the GraphQL request without write permission"
                        .into(),
                );
            }
            Ok(serde_json::from_str(&body)?)
        })
        .transpose()?;

    *request.headers_mut() = prepared_headers;
    mutation::apply_context_mutations(context, prepared_context);
    if let Some(prepared_body) = prepared_body {
        *request.body_mut() = prepared_body;
    }
    Ok(())
}

pub(in crate::plugins::wasm) fn break_supergraph_response(
    context: &Context,
    response: wit::BreakResponse,
) -> Result<supergraph::Response, BoxError> {
    let body: graphql::Response = serde_json::from_str(&response.body)?;
    let mut result = supergraph::Response::new_from_graphql_response(body, context.clone());
    *result.response.status_mut() = http::StatusCode::from_u16(response.status_code)?;
    append_response_headers(result.response.headers_mut(), response.headers)?;
    Ok(result)
}

pub(in crate::plugins::wasm) fn break_subgraph_response(
    request: &subgraph::Request,
    response: wit::BreakResponse,
) -> Result<subgraph::Response, BoxError> {
    let body: graphql::Response = serde_json::from_str(&response.body)?;
    let mut response_builder = http::Response::builder().status(response.status_code);
    for header in response.headers {
        let name = http::HeaderName::try_from(header.name)?;
        for value in header.values {
            response_builder =
                response_builder.header(name.clone(), http::HeaderValue::try_from(value)?);
        }
    }
    Ok(subgraph::Response::new_from_response(
        response_builder.body(body)?,
        request.context.clone(),
        request.subgraph_name.clone(),
        request.id.clone(),
    ))
}

fn append_response_headers(
    target: &mut http::HeaderMap,
    headers: Vec<wit::Header>,
) -> Result<(), BoxError> {
    for header in headers {
        let name = http::HeaderName::try_from(header.name)?;
        for value in header.values {
            target.append(name.clone(), http::HeaderValue::try_from(value)?);
        }
    }
    Ok(())
}
