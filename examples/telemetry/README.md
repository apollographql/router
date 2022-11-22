# Telemetry

Demonstrates configuring of the router for:

- OpenTelemetry
  - Jaeger
  - OpenTelemetry Collector
- Spaceport (Apollo Studio)

## OpenTelemetry

```bash
cargo run -- -s ../../graphql/supergraph.graphql -c ./jaeger.router.yaml
```

## OpenTelemetry Collector

```bash
cargo run -- -s ../../graphql/supergraph.graphql -c ./oltp.router.yaml
```
