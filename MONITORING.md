# Apollo Router Monitoring Setup

This directory contains a complete monitoring stack for the Apollo Router using Prometheus and Grafana.

## Quick Start

1. Start the monitoring stack:
   ```bash
   docker-compose -f monitoring-stack.yml up -d
   ```

2. Start the router with metrics enabled:
   ```bash
   cargo run -- --dev --config router.yaml --supergraph supergraph.graphql
   ```

3. Access the dashboards:
   - Grafana: http://localhost:3000 (admin/admin)
   - Prometheus: http://localhost:9091

## Dashboard Configuration

### Auto-loaded Dashboards

The monitoring stack automatically loads dashboards from `grafana/dashboards/`:

- `router-metrics.json` - Main Apollo Router metrics dashboard

### Manual Dashboard Import

If you create custom dashboards:

1. Export from Grafana: Dashboard Settings → Export → Save to file
2. Save the JSON file in `grafana/dashboards/`
3. Restart Grafana container to load new dashboards

### Key Metrics to Monitor

- **Request Rate**: `rate(apollo_router_http_requests_total[5m])`
- **Response Time**: `histogram_quantile(0.95, rate(apollo_router_http_request_duration_seconds_bucket[5m]))`
- **Error Rate**: `rate(apollo_router_http_requests_total{status=~"5.."}[5m])`
- **Active Connections**: `apollo_router_http_connections_active`

## Load Testing Integration

The monitoring stack works seamlessly with the load testing script:

```bash
# Run 30-minute load test at 10 RPS while monitoring
./run-load-test.sh 30 10
```

View real-time metrics in Grafana during the test to analyze router performance under load.