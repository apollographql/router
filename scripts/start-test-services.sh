#!/usr/bin/env bash
# Start services required for router tests (Redis standalone, Redis cluster, Zipkin, Datadog).
# Run from the repository root. Uses Docker Compose when Docker is available;
# otherwise can start standalone Redis locally if redis-server is installed (cluster tests will be skipped).
set -e
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

REDIS_STANDALONE_PORT=6379
REDIS_CLUSTER_PORT=7000
MAX_WAIT=120

check_port() {
  if command -v nc &>/dev/null; then
    nc -z 127.0.0.1 "$1" 2>/dev/null
  elif command -v redis-cli &>/dev/null && [[ "$1" == "6379" || "$1" == "7000" ]]; then
    redis-cli -p "$1" ping 2>/dev/null | grep -q PONG
  else
    return 1
  fi
}

wait_for_port() {
  local port=$1
  local name=${2:-$port}
  local elapsed=0
  while ! check_port "$port"; do
    if [[ $elapsed -ge $MAX_WAIT ]]; then
      echo "Timed out waiting for $name on port $port" >&2
      return 1
    fi
    echo "Waiting for $name on port $port..."
    sleep 5
    elapsed=$((elapsed + 5))
  done
  echo "$name is up (port $port)"
}

# --- Option 1: Docker Compose (full stack: Redis, Redis cluster, Zipkin, Datadog) ---
# If `docker info` hangs, start Docker Desktop first, or the script will fall through to local Redis.
if command -v docker &>/dev/null; then
  if ! docker info &>/dev/null; then
    echo "Docker is not running. Starting Docker Desktop..."
    if [[ "$(uname)" == Darwin ]]; then
      open -a Docker
    fi
    echo "Waiting for Docker daemon (up to ${MAX_WAIT}s)..."
    elapsed=0
    while ! docker info &>/dev/null; do
      if [[ $elapsed -ge $MAX_WAIT ]]; then
        echo "Docker did not become ready. Falling back to local Redis for non-cluster tests." >&2
        break
      fi
      echo "  ... waiting"
      sleep 5
      elapsed=$((elapsed + 5))
    done
  fi

  if docker info &>/dev/null; then
    echo "Starting services with Docker Compose..."
    docker compose up -d
    echo "Waiting for standalone Redis (required for most tests)..."
    if wait_for_port $REDIS_STANDALONE_PORT "Redis"; then
      echo "Standalone Redis is ready."
    fi
    echo "Waiting for Redis cluster node (required for cluster tests)..."
    if wait_for_port $REDIS_CLUSTER_PORT "Redis cluster"; then
      echo "Redis cluster node is up. The redis-cluster-startup container may still be initializing the cluster (up to ~2 minutes)."
      echo "If cluster tests fail, wait a bit and run the tests again, or run: docker compose logs redis-cluster-startup"
    fi
    echo "Test services are up. Run: cargo xtask test --no-fail-fast --workspace"
    exit 0
  fi
fi

# --- Option 2: Local Redis only (standalone; cluster tests will be skipped/fail) ---
if command -v redis-server &>/dev/null; then
  if check_port $REDIS_STANDALONE_PORT; then
    echo "Redis is already listening on port $REDIS_STANDALONE_PORT."
  else
    echo "Starting local Redis on port $REDIS_STANDALONE_PORT..."
    redis-server --daemonize yes --port $REDIS_STANDALONE_PORT
    sleep 1
  fi
  if check_port $REDIS_STANDALONE_PORT; then
    echo "Standalone Redis is ready. Redis cluster tests will fail unless you run Docker Compose."
    echo "Run: cargo xtask test --no-fail-fast --workspace"
    exit 0
  fi
fi

echo "Could not start test services. Install Docker and run 'docker compose up -d' from the repo root, or install Redis (e.g. brew install redis) and run redis-server." >&2
exit 1
