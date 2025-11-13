#!/bin/bash

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

ROUTER_BIN="./target/release/router"
TAG_REF="artifact-staging.api.apollographql.com/chris-lee-test-efef09a3459458eb:current-97b0560280ed60a5"
DIGEST_REF="artifact-staging.api.apollographql.com/chris-lee-test-efef09a3459458eb@sha256:f9a2f81175ca0b5368dd721c0cf909e1d051643a8aa9236c1d6373f51e7cb243"

if [ -z "${APOLLO_KEY:-}" ]; then
    echo -e "${RED}Error: APOLLO_KEY environment variable is not set${NC}"
    echo -e "${YELLOW}Please set it with: export APOLLO_KEY=your-key${NC}"
    exit 1
fi

PASSED=0
FAILED=0

run_test() {
    local test_name="$1"
    local command="$2"
    local description="$3"
    
    echo -e "\n${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo -e "${BLUE}Test: ${test_name}${NC}"
    echo -e "${BLUE}Description: ${description}${NC}"
    echo -e "${BLUE}Command: ${command}${NC}"
    echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    
    local log_file="/tmp/router_test_${test_name//[^a-zA-Z0-9]/_}.log"
    
    # Kill any existing router processes
    pkill -f "${ROUTER_BIN}" || true
    sleep 1
    lsof -ti:4000,8088 | xargs kill -9 2>/dev/null || true
    sleep 1
    
    # Start router in background
    eval "${command}" > "${log_file}" 2>&1 &
    local router_pid=$!
    
    # Wait for router to start (max 60 seconds for OCI fetches)
    local found_running=false
    local start_time=$(date +%s)
    local max_wait=60
    while true; do
        local current_time=$(date +%s)
        local elapsed=$((current_time - start_time))
        
        if [ ${elapsed} -ge ${max_wait} ]; then
            echo -e "${RED}Timeout after ${max_wait} seconds${NC}"
            break
        fi
        
        sleep 0.5
        
        if grep -q -i "state.*Running\|running" "${log_file}" 2>/dev/null; then
            found_running=true
            break
        fi
        
        # Check if process died (error case)
        if ! kill -0 "${router_pid}" 2>/dev/null; then
            break
        fi
    done
    
    if [ "${found_running}" = true ]; then
        echo -e "${GREEN}✓ PASSED - Router started successfully${NC}"
        echo -e "${GREEN}Found 'running' message in logs${NC}"
        grep -i "state.*Running\|running" "${log_file}" | head -3 || true
        
        # Send SIGINT to stop router
        echo -e "${YELLOW}Sending SIGINT to stop router...${NC}"
        kill -INT "${router_pid}" 2>/dev/null || true
        
        # Wait for graceful shutdown
        for i in {1..10}; do
            sleep 0.5
            if ! kill -0 "${router_pid}" 2>/dev/null; then
                break
            fi
        done
        
        # Force kill if still running
        if kill -0 "${router_pid}" 2>/dev/null; then
            kill -9 "${router_pid}" 2>/dev/null || true
        fi
        
        sleep 2
        ((PASSED++))
        return 0
    else
        echo -e "${RED}✗ FAILED - Router did not start or 'running' message not found${NC}"
        echo -e "${RED}Log output:${NC}"
        tail -30 "${log_file}"
        kill "${router_pid}" 2>/dev/null || true
        sleep 2
        ((FAILED++))
        return 1
    fi
}

echo -e "${GREEN}════════════════════════════════════════════════════════════════════════════════${NC}"
echo -e "${GREEN}Graph Artifact Reference Tests${NC}"
echo -e "${GREEN}════════════════════════════════════════════════════════════════════════════════${NC}"

# Category 4: CLI/Env tests
echo -e "\n${GREEN}Category 4: Graph Artifact Reference via CLI/Env${NC}"

run_test "4.1_tag" "APOLLO_KEY=${APOLLO_KEY} ${ROUTER_BIN} --graph-artifact-reference '${TAG_REF}'" "Graph artifact reference CLI with tag"

run_test "4.2_digest" "APOLLO_KEY=${APOLLO_KEY} ${ROUTER_BIN} --graph-artifact-reference '${DIGEST_REF}'" "Graph artifact reference CLI with digest"

run_test "4.3_tag_hotreload" "APOLLO_KEY=${APOLLO_KEY} ${ROUTER_BIN} --graph-artifact-reference '${TAG_REF}' --hot-reload" "Graph artifact reference CLI with tag and hot reload"

# Category 5: Config file tests
echo -e "\n${GREEN}Category 5: Graph Artifact Reference via Config File${NC}"

TEST_DIR="/tmp/router_test_configs"
mkdir -p "${TEST_DIR}"

cat > "${TEST_DIR}/router_tag_ref.yaml" <<EOF
supergraph:
  listen: 127.0.0.1:4000
graph_artifact_reference: "${TAG_REF}"
EOF

cat > "${TEST_DIR}/router_digest_ref.yaml" <<EOF
supergraph:
  listen: 127.0.0.1:4000
graph_artifact_reference: "${DIGEST_REF}"
EOF

cat > "${TEST_DIR}/router_tag_ref_hotreload.yaml" <<EOF
supergraph:
  listen: 127.0.0.1:4000
graph_artifact_reference: "${TAG_REF}"
hot_reload: true
EOF

run_test "5.1_tag" "APOLLO_KEY=${APOLLO_KEY} ${ROUTER_BIN} --config ${TEST_DIR}/router_tag_ref.yaml" "Graph artifact reference config with tag"

run_test "5.2_digest" "APOLLO_KEY=${APOLLO_KEY} ${ROUTER_BIN} --config ${TEST_DIR}/router_digest_ref.yaml" "Graph artifact reference config with digest"

run_test "5.3_tag_hotreload" "APOLLO_KEY=${APOLLO_KEY} ${ROUTER_BIN} --config ${TEST_DIR}/router_tag_ref_hotreload.yaml" "Graph artifact reference config with tag and hot_reload"

run_test "5.4_tag_cli_hotreload" "APOLLO_KEY=${APOLLO_KEY} ${ROUTER_BIN} --config ${TEST_DIR}/router_tag_ref.yaml --hot-reload" "Graph artifact reference config with tag, CLI hot reload override"

# Cleanup
rm -rf "${TEST_DIR}"

# Summary
echo -e "\n${GREEN}════════════════════════════════════════════════════════════════════════════════${NC}"
echo -e "${GREEN}Test Summary${NC}"
echo -e "${GREEN}════════════════════════════════════════════════════════════════════════════════${NC}"
echo -e "${GREEN}Passed:  ${PASSED}${NC}"
echo -e "${RED}Failed:  ${FAILED}${NC}"
echo -e "${GREEN}════════════════════════════════════════════════════════════════════════════════${NC}"

if [ ${FAILED} -eq 0 ]; then
    echo -e "${GREEN}All tests passed!${NC}"
    exit 0
else
    echo -e "${RED}Some tests failed!${NC}"
    exit 1
fi
