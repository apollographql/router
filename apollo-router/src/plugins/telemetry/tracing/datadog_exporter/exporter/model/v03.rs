use std::time::SystemTime;

use opentelemetry::KeyValue;
use opentelemetry::trace::Status;
use opentelemetry_sdk::error::OTelSdkError;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::trace::SpanData;

use crate::plugins::telemetry::tracing::datadog_exporter::ModelConfig;
use crate::plugins::telemetry::tracing::datadog_exporter::exporter::model::SAMPLING_PRIORITY_KEY;

fn encode_error(e: rmp::encode::ValueWriteError) -> OTelSdkError {
    OTelSdkError::InternalFailure(e.to_string())
}

pub(crate) fn encode<S, N, R>(
    model_config: &ModelConfig,
    traces: Vec<&[SpanData]>,
    get_service_name: S,
    get_name: N,
    get_resource: R,
    resource: Option<&Resource>,
) -> Result<Vec<u8>, OTelSdkError>
where
    for<'a> S: Fn(&'a SpanData, &'a ModelConfig) -> &'a str,
    for<'a> N: Fn(&'a SpanData, &'a ModelConfig) -> &'a str,
    for<'a> R: Fn(&'a SpanData, &'a ModelConfig) -> &'a str,
{
    let mut encoded = Vec::new();
    rmp::encode::write_array_len(&mut encoded, traces.len() as u32).map_err(encode_error)?;

    for trace in traces.into_iter() {
        rmp::encode::write_array_len(&mut encoded, trace.len() as u32).map_err(encode_error)?;

        for span in trace {
            // Safe until the year 2262 when Datadog will need to change their API
            let start = span
                .start_time
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as i64;

            let duration = span
                .end_time
                .duration_since(span.start_time)
                .map(|x| x.as_nanos() as i64)
                .unwrap_or(0);

            let mut span_type_found = false;
            for kv in &span.attributes {
                if kv.key.as_str() == "span.type" {
                    span_type_found = true;
                    rmp::encode::write_map_len(&mut encoded, 12).map_err(encode_error)?;
                    rmp::encode::write_str(&mut encoded, "type").map_err(encode_error)?;
                    rmp::encode::write_str(&mut encoded, kv.value.as_str().as_ref()).map_err(encode_error)?;
                    break;
                }
            }

            if !span_type_found {
                rmp::encode::write_map_len(&mut encoded, 11).map_err(encode_error)?;
            }

            // Datadog span name is OpenTelemetry component name - see module docs for more information
            rmp::encode::write_str(&mut encoded, "service").map_err(encode_error)?;
            rmp::encode::write_str(&mut encoded, get_service_name(span, model_config)).map_err(encode_error)?;

            rmp::encode::write_str(&mut encoded, "name").map_err(encode_error)?;
            rmp::encode::write_str(&mut encoded, get_name(span, model_config)).map_err(encode_error)?;

            rmp::encode::write_str(&mut encoded, "resource").map_err(encode_error)?;
            rmp::encode::write_str(&mut encoded, get_resource(span, model_config)).map_err(encode_error)?;

            rmp::encode::write_str(&mut encoded, "trace_id").map_err(encode_error)?;
            rmp::encode::write_u64(
                &mut encoded,
                u128::from_be_bytes(span.span_context.trace_id().to_bytes()) as u64,
            ).map_err(encode_error)?;

            rmp::encode::write_str(&mut encoded, "span_id").map_err(encode_error)?;
            rmp::encode::write_u64(
                &mut encoded,
                u64::from_be_bytes(span.span_context.span_id().to_bytes()),
            ).map_err(encode_error)?;

            rmp::encode::write_str(&mut encoded, "parent_id").map_err(encode_error)?;
            rmp::encode::write_u64(
                &mut encoded,
                u64::from_be_bytes(span.parent_span_id.to_bytes()),
                ).map_err(encode_error)?;

            rmp::encode::write_str(&mut encoded, "start").map_err(encode_error)?;
            rmp::encode::write_i64(&mut encoded, start).map_err(encode_error)?;

            rmp::encode::write_str(&mut encoded, "duration").map_err(encode_error)?;
            rmp::encode::write_i64(&mut encoded, duration).map_err(encode_error)?;

            rmp::encode::write_str(&mut encoded, "error").map_err(encode_error)?;
            rmp::encode::write_i32(
                &mut encoded,
                match span.status {
                    Status::Error { .. } => 1,
                    _ => 0,
                },
            ).map_err(encode_error)?;

            rmp::encode::write_str(&mut encoded, "meta").map_err(encode_error)?;
            rmp::encode::write_map_len(
                &mut encoded,
                (span.attributes.len() + resource.map(|r| r.len()).unwrap_or(0)) as u32,
            ).map_err(encode_error)?;
            if let Some(resource) = resource {
                for (key, value) in resource.iter() {
                    rmp::encode::write_str(&mut encoded, key.as_str()).map_err(encode_error)?;
                    rmp::encode::write_str(&mut encoded, value.as_str().as_ref()).map_err(encode_error)?;
                }
            }
            for KeyValue { key, value , ..} in span.attributes.iter() {
                rmp::encode::write_str(&mut encoded, key.as_str()).map_err(encode_error)?;
                rmp::encode::write_str(&mut encoded, value.as_str().as_ref()).map_err(encode_error)?;
            }

            rmp::encode::write_str(&mut encoded, "metrics").map_err(encode_error)?;
            rmp::encode::write_map_len(&mut encoded, 1).map_err(encode_error)?;
            rmp::encode::write_str(&mut encoded, SAMPLING_PRIORITY_KEY).map_err(encode_error)?;
            rmp::encode::write_f64(
                &mut encoded,
                if span.span_context.is_sampled() {
                    1.0
                } else {
                    0.0
                },
            ).map_err(encode_error)?;
        }
    }

    Ok(encoded)
}
