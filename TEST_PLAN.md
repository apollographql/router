# Router Startup Test Plan

This document outlines manual test scenarios for router startup verification. Each test should:
1. Start the router with specific configuration
2. Verify router started successfully
3. Verify "running" log message appeared
4. Send interrupt signal (SIGINT/Ctrl+C) to stop the router

## Test Matrix

### Schema Sources
- **File**: `--supergraph <path>` or `APOLLO_ROUTER_SUPERGRAPH_PATH`
- **URLs**: `APOLLO_ROUTER_SUPERGRAPH_URLS` (comma-separated)
- **Uplink**: `APOLLO_KEY` + `APOLLO_GRAPH_REF`
- **Graph Artifact Reference (CLI/env)**: `--graph-artifact-reference <ref>` or `APOLLO_GRAPH_ARTIFACT_REFERENCE`
- **Graph Artifact Reference (config file)**: `graph_artifact_reference` field in YAML config

### Configuration Sources
- **File**: `--config <path>` or `APOLLO_ROUTER_CONFIG_PATH`
- **Default**: No config file (uses defaults)

### Hot Reload
- **Enabled**: `--hot-reload` or `APOLLO_ROUTER_HOT_RELOAD=true`
- **Disabled**: Not set (defaults to false)

## Test Scenarios

### Category 1: File-based Schema (--supergraph)

#### Test 1.1: File schema, no config, no hot reload
```bash
./router --supergraph ./supergraph.graphql
```
**Expected**: Router starts, "running" log appears, can be interrupted

#### Test 1.2: File schema, no config, with hot reload
```bash
./router --supergraph ./supergraph.graphql --hot-reload
```
**Expected**: Router starts, "running" log appears, file watching enabled, can be interrupted

#### Test 1.3: File schema, config file, no hot reload
```bash
./router --supergraph ./supergraph.graphql --config ./router.yaml
```
**Expected**: Router starts, "running" log appears, can be interrupted

#### Test 1.4: File schema, config file, with hot reload
```bash
./router --supergraph ./supergraph.graphql --config ./router.yaml --hot-reload
```
**Expected**: Router starts, "running" log appears, both schema and config watching enabled, can be interrupted

#### Test 1.5: File schema, config file with graph_artifact_reference, no hot reload
**Config file** (`router.yaml`):
```yaml
graph_artifact_reference: "@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
```
```bash
./router --supergraph ./supergraph.graphql --config ./router.yaml
```
**Expected**: Router starts, schema from file takes precedence, config graph_artifact_reference ignored, "running" log appears, can be interrupted

#### Test 1.6: File schema, config file with graph_artifact_reference, with hot reload
**Config file** (`router.yaml`):
```yaml
graph_artifact_reference: "@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
hot_reload: true
```
```bash
./router --supergraph ./supergraph.graphql --config ./router.yaml --hot-reload
```
**Expected**: Router starts, schema from file takes precedence, config graph_artifact_reference ignored, hot reload enabled, "running" log appears, can be interrupted

### Category 2: URL-based Schema (APOLLO_ROUTER_SUPERGRAPH_URLS)

#### Test 2.1: URL schema, no config, no hot reload
```bash
APOLLO_ROUTER_SUPERGRAPH_URLS=http://example.com/schema.graphql ./router
```
**Expected**: Router starts, "running" log appears, can be interrupted

#### Test 2.2: URL schema, no config, with hot reload
```bash
APOLLO_ROUTER_SUPERGRAPH_URLS=http://example.com/schema.graphql ./router --hot-reload
```
**Expected**: Router starts, "running" log appears, URL watching enabled, can be interrupted

#### Test 2.3: URL schema, config file, no hot reload
```bash
APOLLO_ROUTER_SUPERGRAPH_URLS=http://example.com/schema.graphql ./router --config ./router.yaml
```
**Expected**: Router starts, "running" log appears, can be interrupted

#### Test 2.4: URL schema, config file, with hot reload
```bash
APOLLO_ROUTER_SUPERGRAPH_URLS=http://example.com/schema.graphql ./router --config ./router.yaml --hot-reload
```
**Expected**: Router starts, "running" log appears, URL watching enabled, can be interrupted

#### Test 2.5: URL schema, config file with graph_artifact_reference, no hot reload
**Config file** (`router.yaml`):
```yaml
graph_artifact_reference: "@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
```
```bash
APOLLO_ROUTER_SUPERGRAPH_URLS=http://example.com/schema.graphql ./router --config ./router.yaml
```
**Expected**: Router starts, schema from URL takes precedence, config graph_artifact_reference ignored, "running" log appears, can be interrupted

### Category 3: Uplink Schema (APOLLO_KEY + APOLLO_GRAPH_REF)

#### Test 3.1: Uplink schema, no config, no hot reload
```bash
APOLLO_KEY=test-key APOLLO_GRAPH_REF=test@test ./router
```
**Expected**: Router starts, fetches schema from uplink, "running" log appears, can be interrupted

#### Test 3.2: Uplink schema, no config, with hot reload
```bash
APOLLO_KEY=test-key APOLLO_GRAPH_REF=test@test ./router --hot-reload
```
**Expected**: Router starts, fetches schema from uplink, "running" log appears, uplink always reloads, can be interrupted

#### Test 3.3: Uplink schema, config file, no hot reload
```bash
APOLLO_KEY=test-key APOLLO_GRAPH_REF=test@test ./router --config ./router.yaml
```
**Expected**: Router starts, fetches schema from uplink, "running" log appears, can be interrupted

#### Test 3.4: Uplink schema, config file, with hot reload
```bash
APOLLO_KEY=test-key APOLLO_GRAPH_REF=test@test ./router --config ./router.yaml --hot-reload
```
**Expected**: Router starts, fetches schema from uplink, "running" log appears, uplink always reloads, can be interrupted

#### Test 3.5: Uplink schema, config file with graph_artifact_reference, no hot reload
**Config file** (`router.yaml`):
```yaml
graph_artifact_reference: "@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
```
```bash
APOLLO_KEY=test-key APOLLO_GRAPH_REF=test@test ./router --config ./router.yaml
```
**Expected**: Router starts, schema from uplink takes precedence, config graph_artifact_reference ignored, "running" log appears, can be interrupted

### Category 4: Graph Artifact Reference via CLI/Env

#### Test 4.1: Graph artifact reference (CLI), no config, no hot reload
```bash
APOLLO_KEY=test-key ./router --graph-artifact-reference "@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
```
**Expected**: Router starts, fetches schema from OCI registry, "running" log appears, can be interrupted

#### Test 4.2: Graph artifact reference (CLI), no config, with hot reload
```bash
APOLLO_KEY=test-key ./router --graph-artifact-reference "@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef" --hot-reload
```
**Expected**: Router starts, fetches schema from OCI registry, "running" log appears, hot reload enabled, can be interrupted

#### Test 4.3: Graph artifact reference (CLI), config file, no hot reload
```bash
APOLLO_KEY=test-key ./router --graph-artifact-reference "@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef" --config ./router.yaml
```
**Expected**: Router starts, fetches schema from OCI registry, "running" log appears, can be interrupted

#### Test 4.4: Graph artifact reference (CLI), config file, with hot reload
```bash
APOLLO_KEY=test-key ./router --graph-artifact-reference "@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef" --config ./router.yaml --hot-reload
```
**Expected**: Router starts, fetches schema from OCI registry, "running" log appears, hot reload enabled, can be interrupted

#### Test 4.5: Graph artifact reference (env var), no config, no hot reload
```bash
APOLLO_KEY=test-key APOLLO_GRAPH_ARTIFACT_REFERENCE="@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef" ./router
```
**Expected**: Router starts, fetches schema from OCI registry, "running" log appears, can be interrupted

#### Test 4.6: Graph artifact reference (env var), config file, with hot reload
```bash
APOLLO_KEY=test-key APOLLO_GRAPH_ARTIFACT_REFERENCE="@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef" ./router --config ./router.yaml --hot-reload
```
**Expected**: Router starts, fetches schema from OCI registry, "running" log appears, hot reload enabled, can be interrupted

#### Test 4.7: Graph artifact reference (CLI) with different algorithms
```bash
# SHA1
APOLLO_KEY=test-key ./router --graph-artifact-reference "@sha1:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"

# SHA512
APOLLO_KEY=test-key ./router --graph-artifact-reference "@sha512:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"

# MD5
APOLLO_KEY=test-key ./router --graph-artifact-reference "@md5:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"

# Short digest (less than 64 chars)
APOLLO_KEY=test-key ./router --graph-artifact-reference "@sha256:abc123"

# Max algorithm name (32 chars)
APOLLO_KEY=test-key ./router --graph-artifact-reference "@a1234567890123456789012345678901:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
```
**Expected**: Each variant starts successfully, "running" log appears, can be interrupted

### Category 5: Graph Artifact Reference via Config File

#### Test 5.1: Graph artifact reference (config), no hot reload
**Config file** (`router.yaml`):
```yaml
graph_artifact_reference: "@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
```
```bash
APOLLO_KEY=test-key ./router --config ./router.yaml
```
**Expected**: Router starts, fetches schema from OCI registry via config, "running" log appears, can be interrupted

#### Test 5.2: Graph artifact reference (config), with hot reload (CLI)
**Config file** (`router.yaml`):
```yaml
graph_artifact_reference: "@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
```
```bash
APOLLO_KEY=test-key ./router --config ./router.yaml --hot-reload
```
**Expected**: Router starts, fetches schema from OCI registry via config, "running" log appears, hot reload enabled, can be interrupted

#### Test 5.3: Graph artifact reference (config), with hot reload (config file)
**Config file** (`router.yaml`):
```yaml
graph_artifact_reference: "@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
hot_reload: true
```
```bash
APOLLO_KEY=test-key ./router --config ./router.yaml
```
**Expected**: Router starts, fetches schema from OCI registry via config, "running" log appears, hot reload enabled from config, can be interrupted

#### Test 5.4: Graph artifact reference (config), hot reload in both CLI and config
**Config file** (`router.yaml`):
```yaml
graph_artifact_reference: "@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
hot_reload: true
```
```bash
APOLLO_KEY=test-key ./router --config ./router.yaml --hot-reload
```
**Expected**: Router starts, fetches schema from OCI registry via config, "running" log appears, hot reload enabled (CLI takes precedence), can be interrupted

#### Test 5.5: Graph artifact reference (config) with different algorithms
**Config files**:
- `router_sha1.yaml`: `graph_artifact_reference: "@sha1:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"`
- `router_sha512.yaml`: `graph_artifact_reference: "@sha512:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"`
- `router_short.yaml`: `graph_artifact_reference: "@sha256:abc123"`
```bash
APOLLO_KEY=test-key ./router --config ./router_sha1.yaml
APOLLO_KEY=test-key ./router --config ./router_sha512.yaml
APOLLO_KEY=test-key ./router --config ./router_short.yaml
```
**Expected**: Each variant starts successfully, "running" log appears, can be interrupted

### Category 6: Edge Cases and Combinations

#### Test 6.1: Graph artifact reference (CLI) conflicts with file schema
```bash
APOLLO_KEY=test-key ./router --supergraph ./supergraph.graphql --graph-artifact-reference "@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
```
**Expected**: Error - cannot use both file schema and graph artifact reference

#### Test 6.2: Graph artifact reference (CLI) conflicts with URL schema
```bash
APOLLO_KEY=test-key APOLLO_ROUTER_SUPERGRAPH_URLS=http://example.com/schema.graphql ./router --graph-artifact-reference "@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
```
**Expected**: Error - cannot use both URL schema and graph artifact reference

#### Test 6.3: Graph artifact reference (CLI) conflicts with uplink
```bash
APOLLO_KEY=test-key APOLLO_GRAPH_REF=test@test ./router --graph-artifact-reference "@sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
```
**Expected**: Error - cannot use both uplink and graph artifact reference

#### Test 6.4: Graph artifact reference (config) with null value
**Config file** (`router.yaml`):
```yaml
graph_artifact_reference: null
hot_reload: null
```
```bash
APOLLO_KEY=test-key ./router --config ./router.yaml
```
**Expected**: Router starts with default config (no graph artifact reference), "running" log appears, can be interrupted

#### Test 6.5: Graph artifact reference (config) with empty string
**Config file** (`router.yaml`):
```yaml
graph_artifact_reference: ""
```
```bash
APOLLO_KEY=test-key ./router --config ./router.yaml
```
**Expected**: Router starts, empty string treated as None, no OCI fetch, "running" log appears, can be interrupted

#### Test 6.6: Graph artifact reference (config) with invalid format
**Config file** (`router.yaml`):
```yaml
graph_artifact_reference: "invalid-format"
```
```bash
APOLLO_KEY=test-key ./router --config ./router.yaml
```
**Expected**: Router starts but may log error about invalid graph_artifact_reference, "running" log appears, can be interrupted

#### Test 6.7: Hot reload via env var
```bash
APOLLO_ROUTER_HOT_RELOAD=true ./router --supergraph ./supergraph.graphql
```
**Expected**: Router starts, hot reload enabled, "running" log appears, can be interrupted

#### Test 6.8: Hot reload false in config, CLI override
**Config file** (`router.yaml`):
```yaml
hot_reload: false
```
```bash
./router --config ./router.yaml --supergraph ./supergraph.graphql --hot-reload
```
**Expected**: Router starts, hot reload enabled (CLI takes precedence), "running" log appears, can be interrupted

## Test Execution Checklist

For each test:
- [ ] Start router with specified command
- [ ] Verify router process started (check PID)
- [ ] Verify "running" log message appears in output
- [ ] Wait 2-3 seconds to ensure startup completes
- [ ] Send SIGINT (Ctrl+C) to stop router
- [ ] Verify router stops gracefully
- [ ] Document any errors or unexpected behavior

## Notes

- Graph artifact references require `APOLLO_KEY` to be set
- Hot reload affects file watching behavior for both schema and config files
- When multiple schema sources are provided, precedence is: File > URLs > Graph Artifact Reference > Uplink
- Graph artifact reference in config file is only used if no other schema source is provided via CLI/env
- Hot reload CLI flag takes precedence over config file value
- Uplink schema always reloads automatically (independent of hot_reload flag)
