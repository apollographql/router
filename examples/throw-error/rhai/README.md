# Rhai script

Demonstrates different patterns for throwing errors to control request flow, including:

1. **Supergraph-level termination**: Completely terminate client requests based on conditions like missing authentication
2. **Conditional subgraph skipping**: Skip specific subgraphs while maintaining proper GraphQL response structure for federated queries

These patterns are useful for implementing authorization checks, conditional data fetching, and graceful degradation when certain services are unavailable or when required context is missing.

Usage:

```bash
cargo run -- -s ../../graphql/supergraph.graphql -c ./router.yaml
```
