#!/bin/bash

# Apollo Router Load Test Script
# Runs the router, executes drill load test, and modifies schema every 2 seconds
# Usage: ./run-load-test.sh [duration_minutes] [rps]
# Example: ./run-load-test.sh 30 10  (30 minutes at 10 RPS)

set -e

# Parse command line arguments
DURATION_MINUTES=${1:-10}  # Default 10 minutes
RPS=${2:-5}                # Default 5 RPS

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Calculate test parameters
DURATION_SECONDS=$((DURATION_MINUTES * 60))
TOTAL_ITERATIONS=$((RPS * DURATION_SECONDS))
DELAY_MS=$((1000 / RPS))

# Configuration
ROUTER_CONFIG="router.yaml"
SUPERGRAPH_SCHEMA="supergraph.graphql"
DRILL_CONFIG="drill-test-realistic.yml"
SCHEMA_BACKUP="${SUPERGRAPH_SCHEMA}.backup"

# Function to print colored output
print_status() {
    echo -e "${BLUE}[$(date '+%Y-%m-%d %H:%M:%S')]${NC} $1"
}

print_success() {
    echo -e "${GREEN}[$(date '+%Y-%m-%d %H:%M:%S')]${NC} $1"
}

print_warning() {
    echo -e "${YELLOW}[$(date '+%Y-%m-%d %H:%M:%S')]${NC} $1"
}

print_error() {
    echo -e "${RED}[$(date '+%Y-%m-%d %H:%M:%S')]${NC} $1"
}

# Function to cleanup background processes
cleanup() {
    print_status "Cleaning up processes..."
    
    # Kill router process
    if [[ -n $ROUTER_PID ]]; then
        print_status "Stopping router (PID: $ROUTER_PID)..."
        kill $ROUTER_PID 2>/dev/null || true
        wait $ROUTER_PID 2>/dev/null || true
    fi
    
    # Kill drill process
    if [[ -n $DRILL_PID ]]; then
        print_status "Stopping drill test (PID: $DRILL_PID)..."
        kill $DRILL_PID 2>/dev/null || true
        wait $DRILL_PID 2>/dev/null || true
    fi
    
    # Kill schema modifier process
    if [[ -n $SCHEMA_PID ]]; then
        print_status "Stopping schema modifier (PID: $SCHEMA_PID)..."
        kill $SCHEMA_PID 2>/dev/null || true
        wait $SCHEMA_PID 2>/dev/null || true
    fi
    
    # Restore original schema
    if [[ -f "$SCHEMA_BACKUP" ]]; then
        print_status "Restoring original schema..."
        mv "$SCHEMA_BACKUP" "$SUPERGRAPH_SCHEMA"
    fi
    
    print_success "Cleanup completed"
}

# Set trap to cleanup on script exit
trap cleanup EXIT

# Function to check if file exists
check_file() {
    if [[ ! -f "$1" ]]; then
        print_error "File not found: $1"
        exit 1
    fi
}

# Function to wait for router to be ready
wait_for_router() {
    print_status "Waiting for router to be ready..."
    local max_attempts=30
    local attempt=1
    
    while [[ $attempt -le $max_attempts ]]; do
        if curl -s http://localhost:4000/.well-known/apollo/server-health >/dev/null 2>&1; then
            print_success "Router is ready!"
            return 0
        fi
        
        print_status "Attempt $attempt/$max_attempts - Router not ready yet..."
        sleep 2
        ((attempt++))
    done
    
    print_error "Router failed to start within timeout"
    return 1
}

# Function to modify schema every 2 seconds
modify_schema() {
    local counter=1
    while true; do
        sleep 2
        
        # Add a comment to the schema at the very beginning (before schema definition)
        local timestamp=$(date '+%Y-%m-%d %H:%M:%S')
        local comment="# Load test comment $counter added at $timestamp"
        
        # Create temporary file with comment at the top
        {
            echo "$comment"
            cat "$SUPERGRAPH_SCHEMA"
        } > "${SUPERGRAPH_SCHEMA}.tmp"
        
        # Replace original with modified version
        mv "${SUPERGRAPH_SCHEMA}.tmp" "$SUPERGRAPH_SCHEMA"
        
        print_status "Added comment $counter to schema"
        ((counter++))
    done
}

# Main execution
create_drill_config() {
    print_status "Creating dynamic drill configuration"
    print_status "Duration: ${DURATION_MINUTES} minutes (${DURATION_SECONDS} seconds)"
    print_status "Rate: ${RPS} RPS"
    print_status "Total requests: ${TOTAL_ITERATIONS}"
    print_status "Delay between requests: ${DELAY_MS}ms"
    
    cat > "$DRILL_CONFIG" << EOF
---
# Dynamic Drill configuration for Apollo Router load testing
# Generated for ${DURATION_MINUTES} minutes at ${RPS} RPS

concurrency: 1
base: 'http://localhost:4000'
iterations: ${TOTAL_ITERATIONS}
rampup: 60

plan:
  - name: Get Top Products
    weight: 50
    request:
      url: /
      method: POST
      headers:
        Content-Type: application/json
        Accept: application/json
      body: |
        {
          "query": "query GetTopProducts(\$first: Int) { topProducts(first: \$first) { upc name price weight reviews { id body author { username } } } }",
          "variables": { "first": 5 }
        }
    assign:
      - response_time: "{{responseTime}}"

  - name: Get Current User
    weight: 25
    request:
      url: /
      method: POST
      headers:
        Content-Type: application/json
        Accept: application/json
      body: |
        {
          "query": "query GetMe { me { id username name reviews { id body product { upc name price } } } }"
        }
    assign:
      - response_time: "{{responseTime}}"

  - name: Get Recommended Products
    weight: 15
    request:
      url: /
      method: POST
      headers:
        Content-Type: application/json
        Accept: application/json
      body: |
        {
          "query": "query GetRecommendedProducts { recommendedProducts { upc name price weight reviews { id body } } }"
        }
    assign:
      - response_time: "{{responseTime}}"

  - name: Create Review Mutation
    weight: 5
    request:
      url: /
      method: POST
      headers:
        Content-Type: application/json
        Accept: application/json
      body: |
        {
          "query": "mutation CreateReview(\$upc: ID!, \$id: ID!, \$body: String) { createReview(upc: \$upc, id: \$id, body: \$body) { id body author { username } } }",
          "variables": { "upc": "1", "id": "review-123", "body": "Great product!" }
        }
    assign:
      - response_time: "{{responseTime}}"

  - name: Introspection Query
    weight: 5
    request:
      url: /
      method: POST
      headers:
        Content-Type: application/json
        Accept: application/json
      body: |
        {
          "query": "query Introspection { __schema { queryType { name } mutationType { name } } }"
        }
    assign:
      - response_time: "{{responseTime}}"

# Rate limiting to achieve target RPS
delay:
  - ${DELAY_MS}ms

# Reporting and assertions
stats: true
threshold:
  - "response_time < 2000"
  - "status == 200"

# CSV reporting for detailed analysis
csv_report:
  file: drill-results-${DURATION_MINUTES}min-${RPS}rps.csv
  format: 
    - "timestamp"
    - "elapsed"
    - "response_time" 
    - "status"
    - "name"
EOF
}

main() {
    print_status "Starting Apollo Router Load Test"
    
    # Check required files
    check_file "$ROUTER_CONFIG"
    check_file "$SUPERGRAPH_SCHEMA"
    
    # Create dynamic drill configuration
    create_drill_config
    
    # Backup original schema
    cp "$SUPERGRAPH_SCHEMA" "$SCHEMA_BACKUP"
    print_status "Created schema backup: $SCHEMA_BACKUP"
    
    # Start the router
    print_status "Starting Apollo Router..."
    cargo run -- --dev --config "$ROUTER_CONFIG" --supergraph "$SUPERGRAPH_SCHEMA" &
    ROUTER_PID=$!
    print_status "Router started with PID: $ROUTER_PID"
    
    # Wait for router to be ready
    if ! wait_for_router; then
        exit 1
    fi
    
    # Start schema modification in background
    print_status "Starting schema modifier..."
    modify_schema &
    SCHEMA_PID=$!
    print_status "Schema modifier started with PID: $SCHEMA_PID"
    
    # Wait a bit more to ensure router is stable
    sleep 5
    
    # Start drill load test
    print_status "Starting drill load test..."
    print_status "Test will run for ${DURATION_MINUTES} minutes at ${RPS} RPS..."
    
    drill --quiet --benchmark "$DRILL_CONFIG" &
    DRILL_PID=$!
    print_status "Drill test started with PID: $DRILL_PID"
    
    # Monitor the drill process
    print_status "Monitoring load test progress..."
    wait $DRILL_PID
    DRILL_EXIT_CODE=$?
    
    if [[ $DRILL_EXIT_CODE -eq 0 ]]; then
        print_success "Load test completed successfully!"
    else
        print_error "Load test failed with exit code: $DRILL_EXIT_CODE"
    fi
    
    # Keep router running for a bit after test completes
    print_status "Keeping router running for 30 seconds post-test..."
    sleep 30
    
    print_success "Load test session completed"
}

# Help function
show_help() {
    cat << EOF
Apollo Router Load Test Script

Usage: $0 [duration_minutes] [rps]

Parameters:
  duration_minutes  Test duration in minutes (default: 10)
  rps              Requests per second (default: 5)

Examples:
  $0               # 10 minutes at 5 RPS (default)
  $0 30            # 30 minutes at 5 RPS
  $0 30 10         # 30 minutes at 10 RPS
  $0 5 20          # 5 minutes at 20 RPS

The script will:
- Start Apollo Router with cargo run
- Generate dynamic drill configuration
- Run load test at specified rate and duration
- Modify GraphQL schema every 2 seconds
- Generate CSV report: drill-results-{duration}min-{rps}rps.csv
- Clean up and restore original schema on exit
EOF
}

# Check for help flag
if [[ "$1" == "-h" || "$1" == "--help" ]]; then
    show_help
    exit 0
fi

# Validate parameters
if [[ -n "$1" && ! "$1" =~ ^[0-9]+$ ]]; then
    print_error "Duration must be a positive integer (minutes)"
    show_help
    exit 1
fi

if [[ -n "$2" && ! "$2" =~ ^[0-9]+$ ]]; then
    print_error "RPS must be a positive integer"
    show_help
    exit 1
fi

# Check if drill is installed
if ! command -v drill &> /dev/null; then
    print_error "drill command not found. Please install drill load testing tool."
    print_status "Install with: cargo install drill"
    exit 1
fi

# Check if we're in the right directory (should have Cargo.toml)
if [[ ! -f "Cargo.toml" ]]; then
    print_error "Cargo.toml not found. Please run this script from the router project root."
    exit 1
fi

# Run main function
main