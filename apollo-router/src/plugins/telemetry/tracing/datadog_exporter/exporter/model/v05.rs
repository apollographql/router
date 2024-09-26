use std::time::SystemTime;

use opentelemetry::trace::Status;
use opentelemetry_sdk::export::trace::SpanData;

use super::unified_tags::UnifiedTagField;
use super::unified_tags::UnifiedTags;
use crate::plugins::telemetry::tracing::datadog_exporter::exporter::intern::StringInterner;
use crate::plugins::telemetry::tracing::datadog_exporter::exporter::model::DD_MEASURED_KEY;
use crate::plugins::telemetry::tracing::datadog_exporter::exporter::model::SAMPLING_PRIORITY_KEY;
use crate::plugins::telemetry::tracing::datadog_exporter::propagator::SamplingPriority;
use crate::plugins::telemetry::tracing::datadog_exporter::DatadogTraceState;
use crate::plugins::telemetry::tracing::datadog_exporter::Error;
use crate::plugins::telemetry::tracing::datadog_exporter::ModelConfig;

const SPAN_NUM_ELEMENTS: u32 = 12;
const METRICS_LEN: u32 = 2;
const GIT_META_TAGS_COUNT: u32 = if matches!(
    (
        option_env!("DD_GIT_REPOSITORY_URL"),
        option_env!("DD_GIT_COMMIT_SHA")
    ),
    (Some(_), Some(_))
) {
    2
} else {
    0
};

// Protocol documentation sourced from https://github.com/DataDog/datadog-agent/blob/c076ea9a1ffbde4c76d35343dbc32aecbbf99cb9/pkg/trace/api/version.go
//
// The payload is an array containing exactly 12 elements:
//
// 	1. An array of all unique strings present in the payload (a dictionary referred to by index).
// 	2. An array of traces, where each trace is an array of spans. A span is encoded as an array having
// 	   exactly 12 elements, representing all span properties, in this exact order:
//
// 		 0: Service   (uint32)
// 		 1: Name      (uint32)
// 		 2: Resource  (uint32)
// 		 3: TraceID   (uint64)
// 		 4: SpanID    (uint64)
// 		 5: ParentID  (uint64)
// 		 6: Start     (int64)
// 		 7: Duration  (int64)
// 		 8: Error     (int32)
// 		 9: Meta      (map[uint32]uint32)
// 		10: Metrics   (map[uint32]float64)
// 		11: Type      (uint32)
//
// 	Considerations:
//
// 	- The "uint32" typed values in "Service", "Name", "Resource", "Type", "Meta" and "Metrics" represent
// 	  the index at which the corresponding string is found in the dictionary. If any of the values are the
// 	  empty string, then the empty string must be added into the dictionary.
//
// 	- None of the elements can be nil. If any of them are unset, they should be given their "zero-value". Here
// 	  is an example of a span with all unset values:
//
// 		 0: 0                    // Service is "" (index 0 in dictionary)
// 		 1: 0                    // Name is ""
// 		 2: 0                    // Resource is ""
// 		 3: 0                    // TraceID
// 		 4: 0                    // SpanID
// 		 5: 0                    // ParentID
// 		 6: 0                    // Start
// 		 7: 0                    // Duration
// 		 8: 0                    // Error
// 		 9: map[uint32]uint32{}  // Meta (empty map)
// 		10: map[uint32]float64{} // Metrics (empty map)
// 		11: 0                    // Type is ""
//
// 		The dictionary in this case would be []string{""}, having only the empty string at index 0.
//
pub(crate) fn encode<S, N, R>(
    model_config: &ModelConfig,
    traces: Vec<&[SpanData]>,
    get_service_name: S,
    get_name: N,
    get_resource: R,
    unified_tags: &UnifiedTags,
) -> Result<Vec<u8>, Error>
where
    for<'a> S: Fn(&'a SpanData, &'a ModelConfig) -> &'a str,
    for<'a> N: Fn(&'a SpanData, &'a ModelConfig) -> &'a str,
    for<'a> R: Fn(&'a SpanData, &'a ModelConfig) -> &'a str,
{
    let mut interner = StringInterner::new();
    let mut encoded_traces = encode_traces(
        &mut interner,
        model_config,
        get_service_name,
        get_name,
        get_resource,
        &traces,
        unified_tags,
    )?;

    let mut payload = Vec::with_capacity(traces.len() * 512);
    rmp::encode::write_array_len(&mut payload, 2)?;

    interner.write_dictionary(&mut payload)?;

    payload.append(&mut encoded_traces);

    Ok(payload)
}

fn write_unified_tags<'a>(
    encoded: &mut Vec<u8>,
    interner: &mut StringInterner<'a>,
    unified_tags: &'a UnifiedTags,
) -> Result<(), Error> {
    write_unified_tag(encoded, interner, &unified_tags.service)?;
    write_unified_tag(encoded, interner, &unified_tags.env)?;
    write_unified_tag(encoded, interner, &unified_tags.version)?;
    Ok(())
}

fn write_unified_tag<'a>(
    encoded: &mut Vec<u8>,
    interner: &mut StringInterner<'a>,
    tag: &'a UnifiedTagField,
) -> Result<(), Error> {
    if let Some(tag_value) = &tag.value {
        rmp::encode::write_u32(encoded, interner.intern(tag.get_tag_name()))?;
        rmp::encode::write_u32(encoded, interner.intern(tag_value.as_str().as_ref()))?;
    }
    Ok(())
}

fn get_sampling_priority(span: &SpanData) -> f64 {
    match span
        .span_context
        .trace_state()
        .sampling_priority()
        .unwrap_or_else(|| {
            if span.span_context.trace_flags().is_sampled() {
                SamplingPriority::AutoKeep
            } else {
                SamplingPriority::AutoReject
            }
        }) {
        SamplingPriority::UserReject => -1.0,
        SamplingPriority::AutoReject => 0.0,
        SamplingPriority::AutoKeep => 1.0,
        SamplingPriority::UserKeep => 2.0,
    }
}

fn get_measuring(span: &SpanData) -> f64 {
    if span.span_context.trace_state().measuring_enabled() {
        1.0
    } else {
        0.0
    }
}

fn encode_traces<'interner, S, N, R>(
    interner: &mut StringInterner<'interner>,
    model_config: &'interner ModelConfig,
    get_service_name: S,
    get_name: N,
    get_resource: R,
    traces: &'interner [&[SpanData]],
    unified_tags: &'interner UnifiedTags,
) -> Result<Vec<u8>, Error>
where
    for<'a> S: Fn(&'a SpanData, &'a ModelConfig) -> &'a str,
    for<'a> N: Fn(&'a SpanData, &'a ModelConfig) -> &'a str,
    for<'a> R: Fn(&'a SpanData, &'a ModelConfig) -> &'a str,
{
    let mut encoded = Vec::new();
    rmp::encode::write_array_len(&mut encoded, traces.len() as u32)?;

    for trace in traces.iter() {
        rmp::encode::write_array_len(&mut encoded, trace.len() as u32)?;

        for span in trace.iter() {
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

            let mut span_type = interner.intern("");
            for (key, value) in &span.attributes {
                if key.as_str() == "span.type" {
                    span_type = interner.intern_value(value);
                    break;
                }
            }

            // Datadog span name is OpenTelemetry component name - see module docs for more information
            rmp::encode::write_array_len(&mut encoded, SPAN_NUM_ELEMENTS)?;
            rmp::encode::write_u32(
                &mut encoded,
                interner.intern(get_service_name(span, model_config)),
            )?;
            rmp::encode::write_u32(&mut encoded, interner.intern(get_name(span, model_config)))?;
            rmp::encode::write_u32(
                &mut encoded,
                interner.intern(get_resource(span, model_config)),
            )?;
            rmp::encode::write_u64(
                &mut encoded,
                u128::from_be_bytes(span.span_context.trace_id().to_bytes()) as u64,
            )?;
            rmp::encode::write_u64(
                &mut encoded,
                u64::from_be_bytes(span.span_context.span_id().to_bytes()),
            )?;
            rmp::encode::write_u64(
                &mut encoded,
                u64::from_be_bytes(span.parent_span_id.to_bytes()),
            )?;
            rmp::encode::write_i64(&mut encoded, start)?;
            rmp::encode::write_i64(&mut encoded, duration)?;
            rmp::encode::write_i32(
                &mut encoded,
                match span.status {
                    Status::Error { .. } => 1,
                    _ => 0,
                },
            )?;

            rmp::encode::write_map_len(
                &mut encoded,
                (span.attributes.len() + span.resource.len()) as u32
                    + unified_tags.compute_attribute_size()
                    + GIT_META_TAGS_COUNT,
            )?;
            for (key, value) in span.resource.iter() {
                rmp::encode::write_u32(&mut encoded, interner.intern(key.as_str()))?;
                rmp::encode::write_u32(&mut encoded, interner.intern_value(value))?;
            }

            write_unified_tags(&mut encoded, interner, unified_tags)?;

            for (key, value) in span.attributes.iter() {
                rmp::encode::write_u32(&mut encoded, interner.intern(key.as_str()))?;
                rmp::encode::write_u32(&mut encoded, interner.intern_value(value))?;
            }

            if let (Some(repository_url), Some(commit_sha)) = (
                option_env!("DD_GIT_REPOSITORY_URL"),
                option_env!("DD_GIT_COMMIT_SHA"),
            ) {
                rmp::encode::write_u32(&mut encoded, interner.intern("git.repository_url"))?;
                rmp::encode::write_u32(&mut encoded, interner.intern(repository_url))?;
                rmp::encode::write_u32(&mut encoded, interner.intern("git.commit.sha"))?;
                rmp::encode::write_u32(&mut encoded, interner.intern(commit_sha))?;
            }

            rmp::encode::write_map_len(&mut encoded, METRICS_LEN)?;
            rmp::encode::write_u32(&mut encoded, interner.intern(SAMPLING_PRIORITY_KEY))?;
            let sampling_priority = get_sampling_priority(span);
            rmp::encode::write_f64(&mut encoded, sampling_priority)?;

            rmp::encode::write_u32(&mut encoded, interner.intern(DD_MEASURED_KEY))?;
            let measuring = get_measuring(span);
            rmp::encode::write_f64(&mut encoded, measuring)?;
            rmp::encode::write_u32(&mut encoded, span_type)?;
        }
    }

    Ok(encoded)
}
