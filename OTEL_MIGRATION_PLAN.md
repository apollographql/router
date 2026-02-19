# OpenTelemetry SDK 0.31 Migration Plan

This document outlines the changes required to migrate from OpenTelemetry SDK 0.24.x to 0.31.x.

## Overview

The migration involves:
- Dependency updates
- Removing internal forked code in favor of external crates
- API changes throughout the telemetry subsystem
- Architectural changes to handle new lifetime requirements

## Commit Strategy

**Important:** Individual commits may not compile. These are **logical review units** that will be **squashed on merge**. The PR description will note this.

Only the final squashed commit needs to compile and pass tests.

## Phase Execution Requirements

**ESSENTIAL:** For each phase, the following steps MUST be followed:

1. **Fresh Search** - Before making any changes, conduct a fresh search (grep/glob) to discover ALL files that need modification for that phase. Do not rely solely on the file lists in this document - they are starting points only.

2. **Complete All Changes** - Make all necessary modifications for the phase.

3. **Commit Before Next Phase** - Commit all changes with the specified commit message BEFORE starting the next phase. This ensures:
   - Clear separation of concerns for review
   - Ability to bisect issues
   - Logical grouping of related changes

**Search patterns to use per phase type:**
- API changes: `grep -r "old_pattern" --include="*.rs"`
- Import changes: `grep -r "use.*module_name" --include="*.rs"`
- Struct field additions: Search for struct construction sites
- Trait changes: Search for `impl TraitName for`

---

## Phase 1: Dependency Updates

**Search:** `grep -r "opentelemetry" Cargo.toml */Cargo.toml`

**Files:** `Cargo.toml`, `Cargo.lock`

Update all OpenTelemetry crates:
```toml
# Main dependencies
opentelemetry = "0.31"
opentelemetry_sdk = "0.31"
opentelemetry-aws = "0.19"
opentelemetry-http = "0.31"
opentelemetry-jaeger-propagator = "0.31"
opentelemetry-otlp = "0.31"
opentelemetry-semantic-conventions = "0.31"
opentelemetry-zipkin = "0.31"
opentelemetry-prometheus = "0.31"

# Dev dependencies
opentelemetry-stdout = "0.31"
opentelemetry-proto = "0.31"
opentelemetry-datadog = "0.19"
tracing-opentelemetry = "0.32"
```

**Verified:** Versions confirmed from [docs.rs](https://docs.rs) - all core OTel crates at 0.31, datadog/aws at 0.19, tracing-opentelemetry at 0.32 (compatible with OTel 0.31).

**Commit:** `deps: upgrade OpenTelemetry dependencies to 0.31`

---

## Phase 2: Remove Internal Datadog Exporter

**Search:**
- `grep -r "datadog_exporter" --include="*.rs"` - Find all references
- `grep -r "use.*datadog_exporter" --include="*.rs"` - Find imports

**Files to delete:**
- `tracing/datadog_exporter/` (entire directory)

**Files to modify:**
- `tracing/mod.rs` - Remove `datadog_exporter` module declaration
- `tracing/datadog/mod.rs` - Switch to `opentelemetry_datadog::DatadogExporter`

**Keep locally** (custom extensions not in external crate):
- `DatadogTraceState` trait (in propagator module)
- `SamplingPriority` enum (in propagator module)
- `DatadogPropagator` (custom propagator implementation)

**Note:** The internal exporter contains custom `Mapping`, `ModelConfig`, and `FieldMappingFn` for span name mapping. The external crate should provide equivalent functionality - verify during migration.

**Commit:** `refactor: remove internal datadog_exporter, use external crate`

---

## Phase 3: Datadog Sampler/Processor Refactoring

**Search:**
- `grep -r "DatadogAgentSampling\|DatadogSpanProcessor" --include="*.rs"`
- `grep -r "SamplingPriority\|DatadogTraceState" --include="*.rs"`

**Files:** `tracing/datadog/agent_sampling.rs`, `tracing/datadog/span_processor.rs`, `tracing/datadog/mod.rs`

### Sampler (`DatadogAgentSampling`)
Current responsibilities (verified from code):
- Set sampling priority in trace state based on decision
- Respect `parent_based_sampler` config for propagator priority
- Convert `Drop` → `RecordOnly` so spans are always recorded for metrics
- Set `measuring=true` in trace state for Datadog APM metrics
- Add `sampling.priority` attribute for OTLP communication with agent

### Span Processor (`DatadogSpanProcessor`)
Current responsibilities (verified from code):
- Force `sampled=true` flag for all spans to pass batch processor
- The exporter looks at `sampling.priority` attribute for actual sampling

**Verified:** The current implementation has the sampler doing most of the work. The processor only forces `sampled=true`. This separation is intentional and correct.

**Commit:** `refactor: separate Datadog sampler and processor concerns`

---

## Phase 4: Remove Obsolete Code

**Search:**
- `grep -r "named_runtime_channel" --include="*.rs"` - Find all references to remove
- `find . -name "named_runtime_channel.rs"` - Locate file to delete

**Files to delete:**
- `otel/named_runtime_channel.rs` - No longer needed

**Files to modify:**
- `otel/mod.rs` - Remove module declaration

**Keep (verified as used):**
- `error_handler.rs` - Contains:
  - `handle_error()` - Rate-limited OTel error logging
  - `NamedSpanExporter` - Wrapper that prefixes exporter names to errors
  - `NamedMetricsExporter` - Wrapper that prefixes exporter names to metric errors

**Commit:** `chore: remove obsolete telemetry code`

---

## Phase 5: Resource Builder API

**Search:**
- `grep -r "Resource::new\|Resource::from_detectors\|Resource::empty" --include="*.rs"`
- `grep -r "\.with_key_value\|with_attributes" --include="*.rs"` - Check existing patterns

**Files:** `resource.rs` and any file using Resource construction

Old API:
```rust
Resource::new(vec![KeyValue::new("key", "value")])
```

New API:
```rust
Resource::builder_empty()
    .with_attributes([KeyValue::new("key", "value")])
    .build()
```

**Commit:** `fix: update Resource to use builder API`

---

## Phase 6: Key/KeyValue API Changes

**Search:**
- `grep -r "\.string(\|\.array(\|\.i64(\|\.f64(\|\.bool(" --include="*.rs"` - Find Key method calls
- `grep -r "Key::new\|Key::from" --include="*.rs"` - Find Key construction

**Files:** Multiple files throughout telemetry - search will reveal all

Old API:
```rust
Key::new("key").string("value")
Key::new("key").array(vec![...])
```

New API:
```rust
KeyValue::new("key", "value")
KeyValue::new("key", Value::Array(...))
```

**Commit:** `fix: replace Key::string()/array() with KeyValue::new()`

---

## Phase 7: Instrument Builder API

**Search:**
- `grep -r "\.init()" --include="*.rs" | grep -i "counter\|histogram\|gauge"` - Find .init() calls on instruments
- `grep -r "try_init()" --include="*.rs"` - Find try_init() calls

**Files:** Multiple metric-related files - search will reveal all

Old API:
```rust
meter.u64_counter("name").init()
meter.f64_histogram("name").init()
```

New API:
```rust
meter.u64_counter("name").build()
meter.f64_histogram("name").build()
```

**Commit:** `fix: update instrument builders .init() to .build()`

---

## Phase 8: SpanData Struct Changes

**Search:**
- `grep -r "SpanData {" --include="*.rs"` - Find SpanData struct constructions
- `grep -r "SpanData::" --include="*.rs"` - Find SpanData usage

**Files:** `tracing/apollo_telemetry.rs`, `apollo_otlp_exporter.rs` and any file constructing SpanData

Add new required field:
```rust
SpanData {
    // ... existing fields ...
    parent_span_is_remote: false,  // NEW field
}
```

**Answer:** Always `false`. We construct SpanData internally from LightSpanData, not from actual OTel spans with remote parent detection. The router creates all its own spans locally.

**Commit:** `fix: add parent_span_is_remote field to SpanData`

---

## Phase 9: Tracer/TracerProvider Configuration

**Search:**
- `grep -r "TracerProvider\|tracer_provider" --include="*.rs"`
- `grep -r "with_simple_exporter\|with_batch_exporter" --include="*.rs"`
- `grep -r "\.tracer(\|\.tracer_builder(" --include="*.rs"`

**Files:** `reload/tracing.rs`, `otel/tracer.rs` and any file configuring tracers

Update `TracerProvider` construction to use new builder pattern.

**Commit:** `fix: update TracerProvider to new builder API`

---

## Phase 10: SpanExporter Trait Changes

**Search:**
- `grep -r "impl.*SpanExporter" --include="*.rs"` - Find all SpanExporter implementations
- `grep -r "fn export.*SpanData" --include="*.rs"` - Find export method signatures
- `grep -r "BoxFuture.*ExportResult" --include="*.rs"` - Find old return types

**Files:** `tracing/apollo_telemetry.rs`, `tracing/datadog/mod.rs`, `apollo_otlp_exporter.rs` and any SpanExporter impl

### Signature changes:
```rust
// Old
fn export(&mut self, batch: Vec<SpanData>) -> BoxFuture<'static, ExportResult>

// New
fn export(&self, batch: Vec<SpanData>) -> impl Future<Output = OTelSdkResult> + Send
```

### Lifetime fix pattern:
Wrap inner state in `Arc<ExporterInner>` with `tokio::sync::Mutex` around the delegate exporter.

```rust
struct Exporter {
    inner: Arc<ExporterInner>,
}

fn export(&self, spans: Vec<SpanData>) -> BoxFuture<'static, OTelSdkResult> {
    let inner = self.inner.clone();
    async move {
        let exporter = inner.delegate.lock().await;
        exporter.export(spans).await
    }.boxed()
}
```

Apply to:
- `ApolloOtlpExporter`
- `DatadogExporterWrapper`
- `Exporter` (apollo_telemetry)

**Answer for `apollo_telemetry::Exporter`:** Wrap the entire mutable inner state. The export() method mutates:
- `spans_by_parent_id` - LRU cache operations (get_or_insert, get_mut, push)
- `otlp_exporter` - calls export() and shutdown() which need &mut self
- `span_lru_size_instrument` - calls update()

Create an `ExporterInner` struct containing all mutable state and wrap in `Arc<Mutex<ExporterInner>>`.

**Commit:** `fix: SpanExporter lifetime fixes with Arc<Mutex> pattern`

---

## Phase 11: SpanProcessor Trait Changes

**Search:**
- `grep -r "impl.*SpanProcessor" --include="*.rs"` - Find all SpanProcessor implementations
- `grep -r "fn shutdown\|fn force_flush" --include="*.rs"` - Find existing methods

**Files:** `tracing/datadog/span_processor.rs` and any SpanProcessor impl

Add new required method:
```rust
fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
    self.shutdown()
}
```

**Commit:** `fix: add shutdown_with_timeout to SpanProcessor impls`

---

## Phase 12: MetricExporter Changes

**Search:**
- `grep -r "build_metrics_exporter\|MetricsExporter" --include="*.rs"`
- `grep -r "TemporalitySelector\|AggregationSelector" --include="*.rs"`
- `grep -r "Temporality::" --include="*.rs"`

**Files:** `metrics/apollo/mod.rs`, `metrics/otlp.rs` and any metric exporter configuration

Old API:
```rust
.build_metrics_exporter(
    Box::new(CustomTemporalitySelector(...)),
    Box::new(CustomAggregationSelector::builder().boundaries(...).build()),
)?
```

New API:
```rust
.with_temporality(Temporality::Delta)
.build()?
```

**Verified:** `Temporality::Delta` is correct for Apollo metrics. Confirmed from existing code that uses `DeltaTemporalitySelector` for Apollo metric exporters.

**Commit:** `fix: update MetricExporter to new temporality API`

---

## Phase 13: Metric Views Configuration

**Search:**
- `grep -r "with_view\|new_view\|View" --include="*.rs"`
- `grep -r "FilterMeterProvider\|MeterProviderBuilder" --include="*.rs"`
- `grep -r "ExplicitBucketHistogram\|boundaries" --include="*.rs"`

**Files:** `metrics/filter.rs`, `reload/metrics.rs` and any view configuration

- Wire up filter views (`public_view`, `apollo_view`, `apollo_realtime_view`) to meter providers
- Add histogram bucket configuration (`APOLLO_HISTOGRAM_BUCKETS`) to Apollo views
- Fix allocation metrics view (build Stream inside closure)

```rust
const APOLLO_HISTOGRAM_BUCKETS: &[f64] = &[
    0.001, 0.005, 0.015, 0.05, 0.1, 0.2, 0.3, 0.4, 0.5, 1.0, 5.0, 10.0,
];
```

**Verified:** These are the correct histogram bucket values. Confirmed from `config.rs:132-134` where they are the default bucket boundaries for metrics.

**Commit:** `fix: wire up metric filter views to meter providers`

---

## Phase 14: Apollo Telemetry Exporter Refactoring

**Search:**
- `grep -r "extract_traces\|extract_data_from_spans\|extract_root_traces\|group_by_trace" --include="*.rs"`
- `grep -r "impl.*Exporter\|struct Exporter" --include="*.rs" | grep -i apollo`

**Files:** `tracing/apollo_telemetry.rs` and any callers of these methods

Convert methods to static `*_inner` variants that take mutable references as parameters:
- `extract_traces` → `extract_traces_inner`
- `extract_data_from_spans` → `extract_data_from_spans_inner`
- `extract_root_traces` → `extract_root_traces_inner`
- `group_by_trace` → `group_by_trace_inner`

**Commit:** `refactor: extract Apollo telemetry static methods`

---

## Phase 15: ApolloOtlpExporter Cleanup

**Search:**
- `grep -r "ApolloOtlpExporter" --include="*.rs"` - Find all usages
- `grep -r "batch_config\|apollo_key" --include="*.rs" | grep -i otlp` - Find unused field references

**Files:** `apollo_otlp_exporter.rs` and any callers

- Remove unused fields (`batch_config`, `endpoint`, `apollo_key`)
- Split construction into helper methods
- Unify span preparation logic

**Commit:** `refactor: clean up ApolloOtlpExporter`

---

## Phase 16: OTLP Configuration Changes

**Search:**
- `grep -r "opentelemetry_otlp\|OtlpExporter" --include="*.rs"`
- `grep -r "with_tonic\|with_http\|with_endpoint" --include="*.rs"`
- `grep -r "SpanExporterBuilder\|MetricsExporterBuilder" --include="*.rs"`

**Files:** `otlp.rs` and any OTLP configuration

Update OTLP exporter configuration for new builder API patterns.

**Commit:** `fix: update OTLP configuration for new SDK API`

---

## Phase 17: Zipkin Exporter Updates

**Search:**
- `grep -r "opentelemetry_zipkin\|ZipkinExporter" --include="*.rs"`
- `grep -r "zipkin" --include="*.rs"`

**Files:** `tracing/zipkin.rs` and any Zipkin configuration

Update to new `opentelemetry-zipkin` API.

**Commit:** `fix: update Zipkin exporter for new SDK API`

---

## Phase 18: Observable Gauge Lifecycle Management

**Search:**
- `grep -r "ObservableGauge\|observable_gauge" --include="*.rs"`
- `grep -r "with_callback" --include="*.rs"`
- `grep -r "AggregateMeterProvider" --include="*.rs"`

**Problem:** Observable gauges register callbacks that persist globally. When dropped, callbacks remain registered causing memory leaks and stale data.

**Solution:** Handle internally in `AggregateMeterProvider`:
1. Intercept observable gauge creation
2. Create regular gauge + store callback in registry
3. Return wrapper that unregisters on drop
4. Background task invokes callbacks every N seconds

**Files:** `metrics/aggregation.rs`

**Answers:**
1. **Update interval:** Hardcode to 10 seconds. This aligns with existing patterns (e.g., Redis metrics collector uses 5-second intervals). Configuration adds complexity without clear benefit.
2. **Background task spawning:** Spawn lazily on first gauge registration. This avoids creating unnecessary resources when no observable gauges are used.

**Current observable gauge usage patterns (from codebase):**
- `cache/storage.rs`: Cache size and estimated storage gauges using `Arc<AtomicI64>`
- `cache/metrics.rs`: Redis metrics gauges (queue length, latency, etc.)
- Uses `with_callback()` capturing atomic values

**Commit:** `fix: observable gauge lifecycle management in AggregateMeterProvider`

---

## Testing Strategy

After all phases complete:
1. `cargo build` - Ensure compilation
2. `cargo test` - Run unit tests
3. `cargo clippy` - Check for warnings

Integration testing:
- Verify traces reach Apollo Studio
- Verify metrics reach Prometheus/OTLP endpoints
- Verify Datadog integration works
- Verify Zipkin integration works
