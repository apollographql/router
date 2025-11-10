# Testing Strategy for Async Configuration and Schema Loading

## Current Architecture

1. **Configuration file parsing**: Happens synchronously when the stream is created
   - `read_config()` parses the YAML file immediately
   - `UpdateConfiguration` event is emitted synchronously

2. **Supergraph schema loading**: Happens asynchronously after config parsing
   - If config contains `graph_artifact_reference`, schema is fetched from OCI
   - `UpdateSchema` events are emitted asynchronously after config events

## Testing Strategy

### 1. **Event Sequence Testing**
   - Collect all events from the stream using `stream.collect().await`
   - Verify event order: `UpdateConfiguration` must come before `UpdateSchema`
   - Verify stream ends with `NoMoreConfiguration`

### 2. **State Verification Testing**
   - After collecting all events, verify:
     - At least one `UpdateConfiguration` event was emitted
     - If config has `graph_artifact_reference`, at least one `UpdateSchema` event was emitted
     - Config Arc contains valid, parsed configuration
     - Schema SDL is present and valid (if schema loading occurred)

### 3. **Content Validation**
   - Extract config from `UpdateConfiguration` events
   - Verify expected config fields are present and correct
   - Verify `graph_artifact_reference` is set if expected
   - Verify `hot_reload` setting is correct

### 4. **Async Schema Loading Tests**
   - Test with config containing `graph_artifact_reference`
   - Mock OCI fetch to control timing and results
   - Verify schema events appear after config events
   - Test error cases (missing APOLLO_KEY, invalid reference, network errors)

### 5. **File Watching Tests**
   - Test that file changes trigger new `UpdateConfiguration` events
   - Test that schema reloads when config changes (if `hot_reload` is enabled)
   - Test that invalid config changes are ignored

### 6. **Integration-Style Tests**
   - Test the full event stream processing
   - Don't test individual event timing, test final state
   - Use `collect().await` to gather all events, then validate

## Example Test Pattern

```rust
#[tokio::test]
async fn test_config_with_schema_loading() {
    // Setup: Create config file with graph_artifact_reference
    let (path, mut file) = create_temp_file();
    let config_yaml = r#"
        supergraph:
          listen: 127.0.0.1:0
        graph_artifact_reference: "test-ref"
    "#;
    write_and_flush(&mut file, config_yaml).await;
    
    // Set up OCI mock or test environment
    std::env::set_var("APOLLO_KEY", "test-key");
    
    // Create stream
    let stream = ConfigurationSource::File { path, watch: false }
        .into_stream(Some(UplinkConfig::default()), false);
    
    // Collect ALL events (don't test timing, test final state)
    let events: Vec<_> = stream.collect().await;
    
    // Verify event sequence
    let config_idx = events.iter().position(|e| matches!(e, UpdateConfiguration(_)));
    let schema_idx = events.iter().position(|e| matches!(e, UpdateSchema(_)));
    let no_more_idx = events.iter().position(|e| matches!(e, NoMoreConfiguration));
    
    // Config must come before schema, both before NoMoreConfiguration
    assert!(config_idx.is_some(), "Config event should be present");
    if let (Some(c_idx), Some(s_idx)) = (config_idx, schema_idx) {
        assert!(c_idx < s_idx, "Config should come before schema");
    }
    if let (Some(c_idx), Some(n_idx)) = (config_idx, no_more_idx) {
        assert!(c_idx < n_idx, "Config should come before NoMoreConfiguration");
    }
    
    // Verify config content
    if let Some(UpdateConfiguration(config)) = events.iter().find(|e| matches!(e, UpdateConfiguration(_))) {
        assert_eq!(config.graph_artifact_reference.as_ref(), Some(&"test-ref".to_string()));
    }
    
    // Verify schema was loaded
    if let Some(UpdateSchema(schema)) = events.iter().find(|e| matches!(e, UpdateSchema(_))) {
        assert!(!schema.sdl.is_empty(), "Schema SDL should be present");
    }
}
```

## Key Principles

1. **Don't test timing**: Test final state, not when events arrive
2. **Collect all events**: Use `collect().await` to gather complete event sequence
3. **Verify sequence**: Ensure events arrive in correct order
4. **Verify content**: Check that loaded config/schema have expected values
5. **Test async behavior**: Use mocks/stubs for OCI to control async operations
6. **Test error cases**: Verify graceful handling of failures
