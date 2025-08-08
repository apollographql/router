# Apollo Router Diagnostics Plugin

A comprehensive diagnostics and profiling plugin for Apollo Router with jemalloc memory profiling, archive export, and modular architecture.

**Platform Support**: This plugin is only available on Linux platforms due to its dependency on Linux-specific jemalloc features.

## Architecture

The diagnostics plugin follows a modular design with clean separation of concerns:

```
diagnostics/
├── mod.rs                  # Main plugin registration and configuration
├── service.rs              # HTTP routing and authentication
├── memory/                 # Memory profiling functionality
│   └── mod.rs             # Memory service with jemalloc integration
└── export/                # Archive generation for diagnostics data
    ├── mod.rs             # Export service and archive creation
    └── tests.rs           # Comprehensive export functionality tests
```

## Features

### ✅ Memory Profiling (jemalloc)
- Real-time memory profiling control
- Heap dump generation with configurable output directory
- Linux-specific jemalloc integration using CString FFI
- Async I/O with tokio spawn_blocking for non-blocking operations

### ✅ Comprehensive Export System
- Tar.gz archive generation with all diagnostic data
- Modular contribution system for extensibility
- Memory dumps organized in `memory/` subdirectory
- Complete diagnostic manifest with metadata

### ✅ Security & Authentication
- Bearer token authentication with base64 encoding
- Configurable shared secret requirement
- Platform validation with helpful error messages

## Building and running

To build the release router with debug symbols use:
`cargo build --profile=profiling`

To run the router with jemalloc profiling available but disabled on startup:
`_RJEM_MALLOC_CONF=prof:true,prof_active:false target/profiling/router --config router.yaml`

## Configuration

Enable the plugin in your router configuration:

```yaml
plugins:
  experimental_diagnostics:
    enabled: true
    listen: "127.0.0.1:8089"
    shared_secret: "your-secret-here"
    output_directory: "/tmp/router-diagnostics"  # Optional, defaults to "/tmp/router-diagnostics"
```

### Configuration Options

- `enabled` (bool): Enable/disable the diagnostics plugin (default: `false`)
  - **Note**: Only supported on Linux platforms. Enabling on other platforms will cause startup failure.
- `listen` (string): Socket address to bind diagnostics endpoints (default: `"127.0.0.1:8089"`)
- `shared_secret` (string): Authentication secret for accessing endpoints (required when enabled)
- `output_directory` (string): Base directory path for diagnostic files (default: `"/tmp/router-diagnostics"`)
  - Memory dumps are stored in `output_directory/memory/` subdirectory
  - Directory structure is created automatically if it doesn't exist

## API Endpoints

All endpoints require authentication via `Authorization: Bearer <base64(secret)>` header.

### Memory Profiling

- `GET /diagnostics/memory/status` - Get current jemalloc profiling status
- `POST /diagnostics/memory/start` - Start jemalloc memory profiling  
- `POST /diagnostics/memory/stop` - Stop jemalloc memory profiling
- `POST /diagnostics/memory/dump` - Generate heap dump to configured directory

### Export System

- `GET /diagnostics/export` - Download comprehensive tar.gz archive with all diagnostic data

## Authentication

All endpoints require Bearer token authentication:

```bash
# Get profiling status
curl -H "Authorization: Bearer $(echo -n 'your-secret-here' | base64)" \
     http://127.0.0.1:8089/diagnostics/memory/status

# Start profiling
curl -X POST \
     -H "Authorization: Bearer $(echo -n 'your-secret-here' | base64)" \
     http://127.0.0.1:8089/diagnostics/memory/start

# Generate heap dump
curl -X POST \
     -H "Authorization: Bearer $(echo -n 'your-secret-here' | base64)" \
     http://127.0.0.1:8089/diagnostics/memory/dump

# Download full diagnostic archive
curl -H "Authorization: Bearer $(echo -n 'your-secret-here' | base64)" \
     -o diagnostics.tar.gz \
     http://127.0.0.1:8089/diagnostics/export
```

## Export Archive Structure

The `/diagnostics/export` endpoint creates a comprehensive tar.gz archive containing:

```
router-diagnostics-<timestamp>.tar.gz
├── manifest.txt              # Diagnostic metadata and file listing
├── memory/                   # Memory profiling data
│   ├── router_heap_dump_*.prof  # Jemalloc heap dumps
│   └── ...                   # Other memory diagnostic files
└── router-binary             # Current router executable (for analysis)
```

### Manifest Contents

The manifest includes:
- Archive generation timestamp
- Router version information  
- Platform details (Linux)
- Memory output directory path
- File listings with sizes
- Module information and capabilities

## Memory Profiling Integration

The plugin provides complete jemalloc integration:

### Implementation Details
- **CString FFI**: Proper C string handling for jemalloc `mallctl` calls
- **Async Operations**: All blocking jemalloc calls use `tokio::spawn_blocking`
- **Error Handling**: Comprehensive error types using `thiserror`
- **File Management**: Automatic directory creation and proper path handling
- **Timestamped Dumps**: Heap dumps include Unix timestamp in filename

### Heap Dump Generation Process
1. Creates `output_directory/memory/` subdirectory if it doesn't exist
2. Generates timestamped filename: `router_heap_dump_{timestamp}.prof`
3. Uses `tikv-jemalloc-sys::mallctl` with `prof.dump` control
4. Stores file in `output_directory/memory/router_heap_dump_{timestamp}.prof`
5. Returns full dump path in JSON response

## Development

### Platform Requirements
- **Linux Only**: Plugin uses Linux-specific jemalloc features
- Attempting to enable on other platforms results in startup error with clear messaging

### Adding New Diagnostic Modules

1. Create a new submodule directory (e.g., `performance/`)
2. Implement the service handlers following the pattern in `memory/mod.rs`  
3. Add routing logic in `service.rs`
4. Implement `add_to_archive()` method for export contribution
5. Update the main export service to call the new module

Example module integration:
```rust
// In new module (e.g., performance/mod.rs)
impl PerformanceService {
    pub(super) fn add_to_archive<W: std::io::Write>(
        tar: &mut tar::Builder<W>, 
        config: &Config
    ) -> Result<(), BoxError> {
        // Add performance data to "performance/" subdirectory
        let performance_path = Path::new(&config.output_directory).join("performance");
        tar.append_dir_all("performance", &performance_path)?;
        Ok(())
    }
}

// In export/mod.rs
memory::MemoryService::add_to_archive(&mut tar, &config.output_directory)?;
performance::PerformanceService::add_to_archive(&mut tar, config)?;
```

### Testing

```bash
# Run all diagnostics tests (Linux only)
cargo test --package apollo-router diagnostics

# Run memory-specific tests  
cargo test --package apollo-router diagnostics::memory

# Run export-specific tests
cargo test --package apollo-router diagnostics::export

# Run main plugin tests
cargo test --package apollo-router diagnostics::tests
```

### Test Coverage

The plugin includes comprehensive test coverage:
- **Memory Module**: Status, start/stop, heap dump generation
- **Export Module**: Archive creation, manifest generation, empty directories
- **Main Plugin**: Configuration validation, endpoint registration, authentication
- **Platform Support**: Cross-platform compatibility testing

## Error Handling

The plugin provides robust error handling:
- Platform validation with clear error messages
- Configuration validation (shared secret requirement)
- Graceful handling of missing directories
- Proper jemalloc error code handling
- Task execution error handling with context

## Security Considerations

- All endpoints require authentication
- Secrets are validated during plugin initialization
- File paths are properly validated to prevent directory traversal
- Binary files are handled securely in archives
- No sensitive data is logged or exposed

## Future Enhancements

- **Additional Profiling**: CPU profiling, allocation tracking
- **Performance Metrics**: Request latency, throughput analysis  
- **System Monitoring**: Resource utilization, health checks
- **Real-time Streaming**: WebSocket support for live profiling data
- **Configuration APIs**: Dynamic configuration updates
- **Multi-platform Support**: Extend beyond Linux with platform-specific implementations