#!/bin/bash

# Start monitoring stack for Apollo Router

echo "🚀 Starting monitoring stack..."

# Start Prometheus and Grafana
docker-compose -f monitoring-stack.yml up -d

echo "⏳ Waiting for services to start..."
sleep 10

echo "✅ Monitoring stack started!"
echo ""
echo "🔗 Access points:"
echo "   Prometheus: http://localhost:9091"
echo "   Grafana:    http://localhost:3000 (admin/admin)"
echo ""
echo "📊 Common Apollo Router metrics to explore:"
echo "   - apollo_router_http_requests_total"
echo "   - apollo_router_http_request_duration_seconds"
echo "   - apollo_router_query_planning_duration_seconds"
echo "   - apollo_router_subgraph_request_duration_seconds"
echo ""
echo "💡 To stop: docker-compose -f monitoring-stack.yml down"