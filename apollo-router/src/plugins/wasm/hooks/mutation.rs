use http::HeaderName;
use http::HeaderValue;
use tower::BoxError;

use super::super::config::WasmNameMatcher;
use super::super::wit;
use crate::Context;

pub(super) fn externalize_headers(
    source: &http::HeaderMap,
    allowed: &WasmNameMatcher,
) -> Vec<wit::Header> {
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

pub(super) fn externalize_context(
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

pub(super) fn prepare_header_mutations(
    source: &http::HeaderMap,
    allowed: &WasmNameMatcher,
    operations: Vec<wit::HeaderOperation>,
) -> Result<http::HeaderMap, BoxError> {
    let mut headers = source.clone();
    apply_header_mutations(&mut headers, allowed, operations)?;
    Ok(headers)
}

pub(in crate::plugins::wasm) fn apply_header_mutations(
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

pub(super) struct PreparedContextMutations(Vec<PreparedContextMutation>);

enum PreparedContextMutation {
    Set(String, serde_json_bytes::Value),
    Remove(String),
}

pub(super) fn prepare_context_mutations(
    allowed: &WasmNameMatcher,
    operations: Vec<wit::ContextOperation>,
) -> Result<PreparedContextMutations, BoxError> {
    operations
        .into_iter()
        .map(|operation| match operation {
            wit::ContextOperation::Set(entry) => {
                ensure_allowed(allowed, &entry.name, "context")?;
                let value: serde_json::Value = serde_json::from_str(&entry.value)?;
                Ok(PreparedContextMutation::Set(
                    entry.name,
                    serde_json_bytes::to_value(value)?,
                ))
            }
            wit::ContextOperation::Remove(name) => {
                ensure_allowed(allowed, &name, "context")?;
                Ok(PreparedContextMutation::Remove(name))
            }
        })
        .collect::<Result<Vec<_>, BoxError>>()
        .map(PreparedContextMutations)
}

pub(super) fn apply_context_mutations(context: &Context, mutations: PreparedContextMutations) {
    for mutation in mutations.0 {
        match mutation {
            PreparedContextMutation::Set(name, value) => {
                context.insert_json_value(name, value);
            }
            PreparedContextMutation::Remove(name) => {
                context.retain(|key, _| key != &name);
            }
        }
    }
}

fn ensure_allowed(matcher: &WasmNameMatcher, name: &str, kind: &str) -> Result<(), BoxError> {
    if matcher.contains(name) {
        Ok(())
    } else {
        Err(format!("wasm plugin attempted to write unauthorized {kind} `{name}`").into())
    }
}
