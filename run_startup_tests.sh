#!/bin/bash

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Test configuration
ROUTER_BIN="./target/release/router"
TEST_DIR="./test_startup"
SCHEMA_FILE="${TEST_DIR}/supergraph.graphql"
CONFIG_FILE="${TEST_DIR}/router.yaml"
CONFIG_WITH_GAR="${TEST_DIR}/router_with_gar.yaml"
CONFIG_WITH_GAR_HOTRELOAD="${TEST_DIR}/router_with_gar_hotreload.yaml"
CONFIG_NULL="${TEST_DIR}/router_null.yaml"
CONFIG_EMPTY="${TEST_DIR}/router_empty.yaml"
CONFIG_INVALID="${TEST_DIR}/router_invalid.yaml"
CONFIG_HOTRELOAD_FALSE="${TEST_DIR}/router_hotreload_false.yaml"
CONFIG_SHA1="${TEST_DIR}/router_sha1.yaml"
CONFIG_SHA512="${TEST_DIR}/router_sha512.yaml"
CONFIG_SHORT="${TEST_DIR}/router_short.yaml"

# Test counters
PASSED=0
FAILED=0
SKIPPED=0

# Cleanup function
cleanup() {
    # Kill any remaining router processes first
    pkill -f "${ROUTER_BIN}" || true
    sleep 1
    pkill -9 -f "${ROUTER_BIN}" || true
    sleep 1
    
    if [ -d "${TEST_DIR}" ]; then
        rm -rf "${TEST_DIR}"
    fi
}

trap cleanup EXIT

# Setup test directory and files
setup_test_files() {
    echo -e "${BLUE}Setting up test files...${NC}"
    mkdir -p "${TEST_DIR}"
    
    # Copy schema file
    cp "${ROUTER_BIN%/router}../apollo-router/testing_schema.graphql" "${SCHEMA_FILE}" 2>/dev/null || \
    cp "./apollo-router/testing_schema.graphql" "${SCHEMA_FILE}"
    
    # Create basic config file (valid YAML)
    cat > "${CONFIG_FILE}" <<EOF
# Basic router configuration
supergraph:
  listen: 127.0.0.1:4000
EOF
    
    # Config with graph_artifact_reference
    cat > "${CONFIG_WITH_GAR}" <<EOF
graph_artifact_reference: "@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
EOF
    
    # Config with graph_artifact_reference and hot_reload
    cat > "${CONFIG_WITH_GAR_HOTRELOAD}" <<EOF
graph_artifact_reference: "@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
hot_reload: true
EOF
    
    # Config with null values (valid YAML with null)
    # Note: hot_reload can't be null in schema, so we only test graph_artifact_reference: null
    cat > "${CONFIG_NULL}" <<EOF
supergraph:
  listen: 127.0.0.1:4000
graph_artifact_reference: null
EOF
    
    # Config with empty string
    cat > "${CONFIG_EMPTY}" <<EOF
graph_artifact_reference: ""
EOF
    
    # Config with invalid format
    cat > "${CONFIG_INVALID}" <<EOF
graph_artifact_reference: "invalid-format"
EOF
    
    # Config with hot_reload false
    cat > "${CONFIG_HOTRELOAD_FALSE}" <<EOF
hot_reload: false
EOF
    
    # Config with SHA1
    cat > "${CONFIG_SHA1}" <<EOF
graph_artifact_reference: "@sha1:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
EOF
    
    # Config with SHA512
    cat > "${CONFIG_SHA512}" <<EOF
graph_artifact_reference: "@sha512:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
EOF
    
    # Config with short digest
    cat > "${CONFIG_SHORT}" <<EOF
graph_artifact_reference: "@sha256:abc123"
EOF
    
    echo -e "${GREEN}Test files created${NC}"
}

# Wait for ports to be free
wait_for_ports() {
    local max_wait=10
    local waited=0
    while [ ${waited} -lt ${max_wait} ]; do
        if ! lsof -i:4000 -i:8088 >/dev/null 2>&1; then
            return 0
        fi
        sleep 1
        ((waited++))
    done
    # Force kill any processes on these ports
    lsof -ti:4000,8088 | xargs kill -9 2>/dev/null || true
    sleep 2
}

# Run a single test
run_test() {
    local test_name="$1"
    local command="$2"
    local expected_result="${3:-success}"  # success, error, or skip
    local description="${4:-}"
    
    echo -e "\n${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo -e "${BLUE}Test: ${test_name}${NC}"
    if [ -n "${description}" ]; then
        echo -e "${BLUE}Description: ${description}${NC}"
    fi
    echo -e "${BLUE}Command: ${command}${NC}"
    echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    
    if [ "${expected_result}" = "skip" ]; then
        echo -e "${YELLOW}⏭️  SKIPPED${NC}"
        ((SKIPPED++))
        return 0
    fi
    
    # Wait for ports to be free before starting
    wait_for_ports
    
    local log_file="${TEST_DIR}/test_${test_name//[^a-zA-Z0-9]/_}.log"
    local pid_file="${TEST_DIR}/test_${test_name//[^a-zA-Z0-9]/_}.pid"
    
    # Start router in background
    eval "${command}" > "${log_file}" 2>&1 &
    local router_pid=$!
    echo "${router_pid}" > "${pid_file}"
    
    # Wait for router to start (max 30 seconds for OCI fetches, 10 seconds otherwise)
    local max_wait=20
    # Increase wait time for OCI/graph artifact tests
    if echo "${command}" | grep -q "graph-artifact-reference\|graph_artifact_reference"; then
        max_wait=60
    fi
    local found_running=false
    local start_time=$(date +%s)
    while true; do
        local current_time=$(date +%s)
        local elapsed=$((current_time - start_time))
        
        if [ ${elapsed} -ge ${max_wait} ]; then
            echo -e "${RED}Timeout after ${max_wait} seconds${NC}"
            break
        fi
        
        sleep 0.5
        
        if grep -q -i "running\|started\|listening\|state.*Running" "${log_file}" 2>/dev/null; then
            found_running=true
            break
        fi
        
        # Check if process died (error case)
        if ! kill -0 "${router_pid}" 2>/dev/null; then
            break
        fi
    done
    
    # Check results based on expected outcome
    if [ "${expected_result}" = "error" ]; then
        # For error cases, router should not start successfully
        if ! kill -0 "${router_pid}" 2>/dev/null; then
            echo -e "${GREEN}✓ PASSED - Router correctly failed to start${NC}"
            ((PASSED++))
            return 0
        else
            echo -e "${RED}✗ FAILED - Router started but should have failed${NC}"
            echo -e "${RED}Log output:${NC}"
            tail -20 "${log_file}"
            kill "${router_pid}" 2>/dev/null || true
            ((FAILED++))
            return 1
        fi
    else
        # For success cases, router should start and show "running"
        if kill -0 "${router_pid}" 2>/dev/null && [ "${found_running}" = true ]; then
            echo -e "${GREEN}✓ PASSED - Router started successfully${NC}"
            echo -e "${GREEN}Found 'running' message in logs${NC}"
            # Show relevant log lines
            grep -i "running\|started\|listening\|state.*Running" "${log_file}" | head -3 || true
            
            # Send SIGINT to stop router
            echo -e "${YELLOW}Sending SIGINT to stop router...${NC}"
            kill -INT "${router_pid}" 2>/dev/null || true
            
            # Wait for graceful shutdown (max 5 seconds)
            for i in {1..10}; do
                sleep 0.5
                if ! kill -0 "${router_pid}" 2>/dev/null; then
                    break
                fi
            done
            
            # Force kill if still running
            if kill -0 "${router_pid}" 2>/dev/null; then
                echo -e "${YELLOW}Force killing router...${NC}"
                kill -9 "${router_pid}" 2>/dev/null || true
            fi
            
            # Wait longer to ensure port is released
            sleep 2
            
            ((PASSED++))
            return 0
        else
            echo -e "${RED}✗ FAILED - Router did not start or 'running' message not found${NC}"
            echo -e "${RED}Log output:${NC}"
            tail -30 "${log_file}"
            kill "${router_pid}" 2>/dev/null || true
            ((FAILED++))
            return 1
        fi
    fi
}

# Main test execution
main() {
    echo -e "${GREEN}════════════════════════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}Router Startup Test Suite${NC}"
    echo -e "${GREEN}════════════════════════════════════════════════════════════════════════════════${NC}"
    
    # Check if router binary exists
    if [ ! -f "${ROUTER_BIN}" ]; then
        echo -e "${RED}Error: Router binary not found at ${ROUTER_BIN}${NC}"
        echo -e "${YELLOW}Building router...${NC}"
        cargo build --release --bin router
    fi
    
    setup_test_files
    
    # Category 1: File-based Schema
    echo -e "\n${GREEN}════════════════════════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}Category 1: File-based Schema (--supergraph)${NC}"
    echo -e "${GREEN}════════════════════════════════════════════════════════════════════════════════${NC}"
    
    run_test "1.1" "${ROUTER_BIN} --supergraph ${SCHEMA_FILE}" "success" "File schema, no config, no hot reload"
    sleep 2  # Extra wait between tests to ensure port release
    run_test "1.2" "${ROUTER_BIN} --supergraph ${SCHEMA_FILE} --hot-reload" "success" "File schema, no config, with hot reload"
    sleep 2
    run_test "1.3" "${ROUTER_BIN} --supergraph ${SCHEMA_FILE} --config ${CONFIG_FILE}" "success" "File schema, config file, no hot reload"
    sleep 2
    run_test "1.4" "${ROUTER_BIN} --supergraph ${SCHEMA_FILE} --config ${CONFIG_FILE} --hot-reload" "success" "File schema, config file, with hot reload"
    sleep 2
    run_test "1.5" "${ROUTER_BIN} --supergraph ${SCHEMA_FILE} --config ${CONFIG_WITH_GAR}" "success" "File schema, config with graph_artifact_reference (should be ignored)"
    sleep 2
    run_test "1.6" "${ROUTER_BIN} --supergraph ${SCHEMA_FILE} --config ${CONFIG_WITH_GAR_HOTRELOAD} --hot-reload" "success" "File schema, config with graph_artifact_reference and hot_reload"
    
    # Category 2: URL-based Schema (skip - requires actual HTTP server)
    echo -e "\n${GREEN}════════════════════════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}Category 2: URL-based Schema (SKIPPED - requires HTTP server)${NC}"
    echo -e "${GREEN}════════════════════════════════════════════════════════════════════════════════${NC}"
    
    run_test "2.1" "APOLLO_ROUTER_SUPERGRAPH_URLS=http://example.com/schema.graphql ${ROUTER_BIN}" "skip" "URL schema - requires HTTP server"
    
    # Category 3: Uplink Schema (skip - requires valid APOLLO_KEY and APOLLO_GRAPH_REF)
    echo -e "\n${GREEN}════════════════════════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}Category 3: Uplink Schema (SKIPPED - requires valid credentials)${NC}"
    echo -e "${GREEN}════════════════════════════════════════════════════════════════════════════════${NC}"
    
    run_test "3.1" "APOLLO_KEY=test-key APOLLO_GRAPH_REF=test@test ${ROUTER_BIN}" "skip" "Uplink schema - requires valid credentials"
    
    # Category 4: Graph Artifact Reference via CLI/Env
    echo -e "\n${GREEN}════════════════════════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}Category 4: Graph Artifact Reference via CLI/Env${NC}"
    echo -e "${GREEN}════════════════════════════════════════════════════════════════════════════════${NC}"
    
    # Test with tag reference
    run_test "4.1_tag" "APOLLO_KEY=${APOLLO_KEY:-test-key} ${ROUTER_BIN} --graph-artifact-reference 'artifact-staging.api.apollographql.com/chris-lee-test-efef09a3459458eb:current-97b0560280ed60a5'" "success" "Graph artifact reference CLI with tag"
    sleep 2
    
    # Test with digest reference
    run_test "4.2_digest" "APOLLO_KEY=${APOLLO_KEY:-test-key} ${ROUTER_BIN} --graph-artifact-reference 'artifact-staging.api.apollographql.com/chris-lee-test-efef09a3459458eb@sha256:f9a2f81175ca0b5368dd721c0cf909e1d051643a8aa9236c1d6373f51e7cb243'" "success" "Graph artifact reference CLI with digest"
    sleep 2
    
    # Test with tag reference and hot reload
    run_test "4.3_tag_hotreload" "APOLLO_KEY=${APOLLO_KEY:-test-key} ${ROUTER_BIN} --graph-artifact-reference 'artifact-staging.api.apollographql.com/chris-lee-test-efef09a3459458eb:current-97b0560280ed60a5' --hot-reload" "success" "Graph artifact reference CLI with tag and hot reload"
    sleep 2
    
    # Test with digest reference and config file
    run_test "4.4_digest_config" "APOLLO_KEY=${APOLLO_KEY:-test-key} ${ROUTER_BIN} --graph-artifact-reference 'artifact-staging.api.apollographql.com/chris-lee-test-efef09a3459458eb@sha256:f9a2f81175ca0b5368dd721c0cf909e1d051643a8aa9236c1d6373f51e7cb243' --config ${CONFIG_FILE}" "success" "Graph artifact reference CLI with digest and config file"
    
    # Category 5: Graph Artifact Reference via Config File
    echo -e "\n${GREEN}════════════════════════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}Category 5: Graph Artifact Reference via Config File${NC}"
    echo -e "${GREEN}════════════════════════════════════════════════════════════════════════════════${NC}"
    
    # Create config files with actual references
    cat > "${TEST_DIR}/router_tag_ref.yaml" <<EOF
supergraph:
  listen: 127.0.0.1:4000
graph_artifact_reference: "artifact-staging.api.apollographql.com/chris-lee-test-efef09a3459458eb:current-97b0560280ed60a5"
EOF
    
    cat > "${TEST_DIR}/router_digest_ref.yaml" <<EOF
supergraph:
  listen: 127.0.0.1:4000
graph_artifact_reference: "artifact-staging.api.apollographql.com/chris-lee-test-efef09a3459458eb@sha256:f9a2f81175ca0b5368dd721c0cf909e1d051643a8aa9236c1d6373f51e7cb243"
EOF
    
    cat > "${TEST_DIR}/router_tag_ref_hotreload.yaml" <<EOF
supergraph:
  listen: 127.0.0.1:4000
graph_artifact_reference: "artifact-staging.api.apollographql.com/chris-lee-test-efef09a3459458eb:current-97b0560280ed60a5"
hot_reload: true
EOF
    
    # Test with tag reference in config
    run_test "5.1_tag" "APOLLO_KEY=${APOLLO_KEY:-test-key} ${ROUTER_BIN} --config ${TEST_DIR}/router_tag_ref.yaml" "success" "Graph artifact reference config with tag"
    sleep 2
    
    # Test with digest reference in config
    run_test "5.2_digest" "APOLLO_KEY=${APOLLO_KEY:-test-key} ${ROUTER_BIN} --config ${TEST_DIR}/router_digest_ref.yaml" "success" "Graph artifact reference config with digest"
    sleep 2
    
    # Test with tag reference and hot reload in config
    run_test "5.3_tag_hotreload" "APOLLO_KEY=${APOLLO_KEY:-test-key} ${ROUTER_BIN} --config ${TEST_DIR}/router_tag_ref_hotreload.yaml" "success" "Graph artifact reference config with tag and hot_reload"
    sleep 2
    
    # Test with tag reference in config and CLI hot reload override
    run_test "5.4_tag_cli_hotreload" "APOLLO_KEY=${APOLLO_KEY:-test-key} ${ROUTER_BIN} --config ${TEST_DIR}/router_tag_ref.yaml --hot-reload" "success" "Graph artifact reference config with tag, CLI hot reload override"
    
    # Category 6: Edge Cases
    echo -e "\n${GREEN}════════════════════════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}Category 6: Edge Cases${NC}"
    echo -e "${GREEN}════════════════════════════════════════════════════════════════════════════════${NC}"
    
    run_test "6.1" "${ROUTER_BIN} --supergraph ${SCHEMA_FILE} --graph-artifact-reference '@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef'" "error" "Graph artifact reference conflicts with file schema"
    run_test "6.4" "${ROUTER_BIN} --config ${CONFIG_NULL} --supergraph ${SCHEMA_FILE}" "success" "Config with null values"
    run_test "6.5" "${ROUTER_BIN} --config ${CONFIG_EMPTY} --supergraph ${SCHEMA_FILE}" "success" "Config with empty string graph_artifact_reference"
    run_test "6.7" "APOLLO_ROUTER_HOT_RELOAD=true ${ROUTER_BIN} --supergraph ${SCHEMA_FILE}" "success" "Hot reload via env var"
    run_test "6.8" "${ROUTER_BIN} --config ${CONFIG_HOTRELOAD_FALSE} --supergraph ${SCHEMA_FILE} --hot-reload" "success" "Hot reload false in config, CLI override"
    
    # Summary
    echo -e "\n${GREEN}════════════════════════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}Test Summary${NC}"
    echo -e "${GREEN}════════════════════════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}Passed:  ${PASSED}${NC}"
    echo -e "${RED}Failed:  ${FAILED}${NC}"
    echo -e "${YELLOW}Skipped: ${SKIPPED}${NC}"
    echo -e "${GREEN}════════════════════════════════════════════════════════════════════════════════${NC}"
    
    if [ ${FAILED} -eq 0 ]; then
        echo -e "${GREEN}All tests passed!${NC}"
        exit 0
    else
        echo -e "${RED}Some tests failed!${NC}"
        exit 1
    fi
}

main "$@"
