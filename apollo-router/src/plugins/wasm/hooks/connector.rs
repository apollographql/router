use apollo_federation::connectors::runtime::http_json_transport::TransportRequest;
use tower::BoxError;

use super::super::config::WasmHookConfig;
use super::super::config::WasmTransportAccess;
use super::super::wit;
use super::mutation;
use crate::Context;
use crate::services::connector::request_service;

pub(in crate::plugins::wasm) fn connector_event(
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
            mutation::externalize_headers(http_request.inner.headers(), &permissions.headers.read),
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
        context: mutation::externalize_context(&request.context, &permissions.context.read)?,
        body,
        configuration: configuration.to_string(),
    })
}

pub(in crate::plugins::wasm) fn apply_connector_mutation(
    transport_request: &mut TransportRequest,
    context: &Context,
    hook: &WasmHookConfig,
    mutation: wit::Mutation,
) -> Result<(), BoxError> {
    let wit::Mutation {
        headers: header_operations,
        context: context_operations,
        method,
        uri,
        body,
    } = mutation;
    if matches!(transport_request, TransportRequest::MappingOnly)
        && (!header_operations.is_empty() || method.is_some() || uri.is_some() || body.is_some())
    {
        return Err(
            "wasm plugin attempted to modify HTTP data for a mapping-only connector".into(),
        );
    }

    let prepared_context =
        mutation::prepare_context_mutations(&hook.permissions.context.write, context_operations)?;

    if let TransportRequest::Http(http_request) = transport_request {
        ensure_transport_write(
            hook.permissions.transport.method,
            method.is_some(),
            "method",
        )?;
        ensure_transport_write(hook.permissions.transport.uri, uri.is_some(), "URI")?;
        ensure_transport_write(hook.permissions.transport.body, body.is_some(), "body")?;

        let prepared_method = method.as_deref().map(http::Method::try_from).transpose()?;
        let prepared_uri = uri
            .as_deref()
            .map(|value| connector_uri(http_request.inner.uri(), value))
            .transpose()?;
        reject_derived_header_mutations(&header_operations)?;
        let mut prepared_headers = mutation::prepare_header_mutations(
            http_request.inner.headers(),
            &hook.permissions.headers.write,
            header_operations,
        )?;
        prepared_headers.remove(http::header::CONTENT_LENGTH);

        *http_request.inner.headers_mut() = prepared_headers;
        if let Some(method) = prepared_method {
            *http_request.inner.method_mut() = method;
        }
        if let Some(uri) = prepared_uri {
            *http_request.inner.uri_mut() = uri;
        }
        if let Some(body) = body {
            *http_request.inner.body_mut() = body;
        }
    }
    mutation::apply_context_mutations(context, prepared_context);
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
