# Metrics

The Router uses OpenTelemetry metrics to support Prometheus and OTLP exporters.

## Requirements
* Filtering of metrics to Public and Private exporters. This is to support Apollo only metrics and to exclude sending of legacy metrics to Apollo.
* Multiple exporters - Prometheus and OTLP.
* Prometheus metrics must persist across reloads.
* Metrics must be testable.

## Entities
```mermaid

erDiagram

    callsite-tracing ||--|{ metrics-layer : uses
    callsite-macro ||--|{ aggregate-meter-provider : uses
    callsite-macro ||--|{ instrument : mutates

    metrics-layer ||--|| aggregate-meter-provider : uses
    metrics-layer ||--|{ instrument : mutates

    telemetry-plugin ||--|| metrics-layer : clears
    telemetry-plugin ||--|| aggregate-meter-provider : configures

    aggregate-meter-provider ||--|| public-filtered-meter-provider : uses
    aggregate-meter-provider ||--|| public-filtered-prometheus-meter-provider : uses
    aggregate-meter-provider ||--|| private-filtered-meter-provider : uses

    public-filtered-meter-provider ||--|{ public-meter-provider : uses
    public-filtered-prometheus-meter-provider ||--|{ public-prometheus-meter-provider : uses
    private-filtered-meter-provider ||--|{ private-meter-provider : uses
    
    public-meter-provider ||--|{ public-meter : creates
    public-prometheus-meter-provider ||--|{ public-prometheus-meter : creates
    private-meter-provider ||--|{ private-meter : creates

    public-meter ||--|{ instrument : creates
    public-prometheus-meter ||--|{ instrument : creates
    private-meter ||--|{ instrument : creates

    instrument

    "exporter(s)" ||--|{ public-meter : observes
    prometheus-exporter ||--|{ public-prometheus-meter : observes
    prometheus-registry ||--|| prometheus-exporter : observes
    private-otlp-exporter ||--|{ private-meter : observes

```

### Instrument
A histogram, counter or gauge that is used to record metrics.

### Meter
Creates instruments, also contains a reference to exporters so that when instruments are created the
* __Public meter__ - Exports to all public metrics to configured exporters except for Prometheus.
* __Public prometheus meter__ - Exports to all public metrics to Prometheus.
* __Private meter__ - Exports to all public metrics to Apollo.


### Meter provider
Creates meters
* __Public meter provider__ - Creates public meters (see above).
* __Public prometheus meter provider__ - Creates public prometheus meters (see above).
* __Private meter provider__ - Creates private meters (see above).

### Filter meter provider
Depending on a meter name will return no-op or delegate to a meter provider. Used to filter public vs private metrics.

### Aggregate meter provider
A meter provider that wraps public, public prometheus, and private meter providers. Used to create a single meter provider that can be used by the metrics layer and metrics macros.
This meter provider is also responsible for maintaining a strong reference to all instruments that are currently valid. This enables [callsite instrument caching](#callsite-instrument-caching).

### Metrics layer
The tracing-opentelemetry layer that is used to create instruments and meters. This will cache instruments after they have been created.

### Metrics macros
New macros that will be used for metrics going forward. Allows unit testing of metrics.

### Prometheus registry
Used to render prometheus metrics. Contains no state.

## Design gotchas
The metrics code is substantial, however there are reasons that it is structured in the way that it is.

1. There is no way to filter instruments at the exporter level. This is the reason that we have aggregate meter providers that wrap the public, public prometheus, and private meter providers. This allows us to filter out private metrics at the meter provider level.
2. The meter provider and meter layer are both globals. This has made testing hard. The new metrics macros should be used as they have built in support for testing by moving the meter provider to a task or thread local.
3. Prometheus meters need to be kept around across reloads otherwise metrics are reset. This is why the aggregate meter provider allows internal mutability.

## Using metrics macros

Metrics macros are a replacement for the tracing-opentelemetry metrics-layer.
They are highly optimised, allow dynamic attributes, are easy to use and support unit testing.

### Usage

There are two classes of instrument, observable and non-observable. Observable instruments will ask for their value when they are exported, non-observable will update at the point of mutation.

Observable gauges are attached to a particular meter, so they MUST be created after the telemetry plugin `activate()` has been called as this is the point where meters will updated.
We're going to have to think about how to make this less brittle.

```rust
// non-observable instruments - good for histograms and counters
u64_counter!("test", "test description", 1, vec![KeyValue::new("attr", "val")]);    
u64_counter!("test", "test description", 1, "attr" => "val");
u64_counter!("test", "test description", 1);

// observable instruments - good for gauges
meter_provider()
  .meter("test")
  .u64_observable_gauge("test")
  .with_callback(|m| m.observe(5, &[]))
  .init();
```

### Testing
When using the macro in a test you will need a different pattern depending on if you are writing a sync or async test.

#### Testing Sync
```rust
   #[test]
    fn test_non_async() {
        // Each test is run in a separate thread, metrics are stored in a thread local.
        u64_counter!("test", "test description", 1, "attr" => "val");
        assert_counter!("test", 1, "attr" => "val");
    }
```

#### Testing Async

Make sure to use `.with_metrics()` method on the async block to ensure that the metrics are stored in a task local.
*Tests will silently fail to record metrics if this is not done.*
```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn test_async_multi() {
        // Multi-threaded runtime needs to use a tokio task local to avoid tests interfering with each other
        async {
            u64_counter!("test", "test description", 1, "attr" => "val");
            assert_counter!("test", 1, "attr" => "val");
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_async_single() {
        async {
            // It's a single threaded tokio runtime, so we can still use a thread local
            u64_counter!("test", "test description", 1, "attr" => "val");
            assert_counter!("test", 1, "attr" => "val");
        }
        .with_metrics()
        .await;
    }
```

## Callsite instrument caching

When using the new metrics macros a reference to an instrument is cached to ensure that the meter provider does not have to be queried over and over.

```mermaid

flowchart TD
    Callsite --> RefCheck
    RefCheck -->|not upgradable| Create
    RefCheck -->|upgradable| Use
    Create --> Store
    Store --> Use
    RefCheck{"Static\nMutex < Weak < Instrument > >"}
    Create("Create instrument Arc < Instrument >")
    Store("Store downgraded clone in Mutex")
    Use("Use strong reference to instrument")
```

Aggregate meter provider is responsible for maintaining a strong reference to all instruments that are valid. 

Strong references to instruments will be discarded when changes to the aggregate meter provider take place. This will cause every callsite to refresh its reference to the instrument.

On the fast path the mutex is locked for the period that it takes to upgrade the weak reference. This is a fast operation, and should not block the thread for any meaningful period of time.

If there is shown to be contention in future profiling we can revisit.
