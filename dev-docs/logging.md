# Logging

The Router uses tokio tracing for logging. When writing code make sure to include log statements that will help users debug their own issues.
To ensure a aconsistent experience for our users, make sure to follow the following guidelines. 

## Guidelines

### Don't use variable interpolation
Log statements should be fixed should not use variable interpolation. This allows users to filter logs by message.

#### Good

```rust
debug!(request, "received request");
```

#### Bad

```rust
debug!("received request: {}", request);
```

### Make the error message short and concise

If actions can be taken to resolve the error include them in an `action` attribute

#### Good
```rust
error!(actions = ["check that the request is valid on the client, and modify the router config to allow the request"], "bad request");
```

#### Bad
```rust
error!(request, "bad request, check that the request is valid on the client, and modify the router config to allow the request");
```

### Use otel attributes
When adding fields to an error message check to see if an attribute already defined in [OpenTelemetry semantic conventions](https://opentelemetry.io/docs/specs/semconv/).
By using these well-defined attributes, APM providers have a better chance of understanding the error.

#### Good
```rust
error!(url.full = url, "bad request");
```

#### Bad
```rust
error!(url, "bad request");
```

### Include caught error as `exception.message` field
`exception.message` is used to capture the error message of a caught error.

See [OpenTelemetry semantic conventions](https://opentelemetry.io/docs/specs/semconv/exceptions/exceptions-logs/) for more information.

#### Good
```rust
error!(exception.message = err, "bad request");
```

#### Bad
```rust
error!("bad request {}", err);
```

### Include error type as `exception.type` field
`exception.type` is used to capture the class of an error message, in our case this translates to error code.

See [OpenTelemetry semantic conventions](https://opentelemetry.io/docs/specs/semconv/exceptions/exceptions-logs/) for more information.

#### Good
```rust
error!(exception.type = err.code(), "bad request");
```

#### Bad
```rust
error!(exception.type = "MyError", "bad request");
```


## Testing
Log statements can be captured during a test using by attaching a subscriber by using `assert_snapshot_subscriber!()`.

Under the hood `insta` is used to assert that a yaml version of the log statements is identical. For example here is the output of a test with three log statements:

Do add tests for logging, it's very low overhead, and the act of seeing log statements in a test can help you think about what you are logging and how to help the user.

```yaml
---
source: apollo-router/src/plugins/authentication/tests.rs
expression: yaml
---
- fields:
    alg: UnknownAlg
    reason: "unknown variant `UnknownAlg`, expected one of `HS256`, `HS384`, `HS512`, `ES256`, `ES384`, `RS256`, `RS384`, `RS512`, `PS256`, `PS384`, `PS512`, `EdDSA`"
    index: 2
  level: WARN
  message: "ignoring a key since it is not valid, enable debug logs to full content"
- fields:
    alg: "<unknown>"
    reason: "invalid value: map, expected map with a single key"
    index: 3
  level: WARN
  message: "ignoring a key since it is not valid, enable debug logs to full content"
- fields:
    alg: ES256
    reason: "invalid type: string \"Hmm\", expected a sequence"
    index: 5
  level: WARN
  message: "ignoring a key since it is not valid, enable debug logs to full content"
```



#### Testing Sync
Use `subscriber::with_default` to attach a subscriber for the duration of a block.

```rust
    #[test]
    async fn test_sync() {
        subscriber::with_default(assert_snapshot_subscriber!(), || { ... })
    }
```

#### Testing Async

Use `with_subscriber` to attach a subscriber to an async block. 

```rust
    #[tokio::test]
    async fn test_async() {
        async{...}.with_subscriber(assert_snapshot_subscriber!())
    }
```


