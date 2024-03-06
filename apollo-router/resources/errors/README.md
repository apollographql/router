This directory contains files that are used to populate static error definitions.
It is recommended that you set up your editor to use the schema.json file in this directory to provide autocompletion and validation for error definitions.


Errors should be defined using the  error defined with the error macro, for example:
```rust
    error_type!("Test error docs", TestError, {
        /// Not found
        NotFound {
            /// Some attribute
            attr: String,
            #[serde(skip)]
            source: Box<dyn std::error::Error + Send + Sync>,
        },
        /// Bad request
        BadRequest,
    });
```

The error macro does the necessary plumping such that the error type will implement `StructuredError`. This trait allows access to static metadata about the error defined in a yaml file.

The yaml file is named using the fully qualified name of the error type, with the error type name in snake case. For example, the error type `my_mod::TestError` would have a corresponding yaml file at `my_package::TestError.yaml`.

Example error definition:
```yaml
- code: TEST_ERROR__BAD_REQUEST
  level: error
  origin: subgraph
  detail: "The request was invalid or cannot be otherwise served."
  type: bad_request
  actions:
    - "Do something"
  attributes:
    name:
        type: string
        description: "The name of the entity."
```

The yaml file contains a list of error definitions. Each error definition should have the following fields:

| Field      | Description                                              | Value                                                                                                                                                                          |  
|------------|----------------------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| code       | The error code                                           | A unique identifier for the error of the following form: <ERROR_TYPE_NAME>__<VARIANT_NAME>                                                                                     |
| level      | The severity of the error                                | One of `error`, `warning`, or `info`                                                                                                                                           |
| origin     | The source of the error                                  | One of `subgraph`, `runtime`, or `user` or a free format string                                                                                                                |
| detail     | A human-readable description of the error                | More information about when and where this error can occur                                                                                                                     |
| type       | A machine-readable description of the error              | One of `bad_request`, `not_found`, `forbidden`, `conflict`, `internal`, `unauthorized`, `unavailable`, `rate_limit`, `timeout`, `validation`, `network`, `service`, `unknown`  |
| actions    | A list of actions that can be taken to resolve the error | A list of actions that can be taken to resolve the error                                                                                                                       |
| attributes | A list of attributes that are associated with the error  | A list of attributes that are associated with the error                                                                                                                        |


Attributes are defined as:

| Field       | Description                                              | Value                                                                            |
|-------------|----------------------------------------------------------|----------------------------------------------------------------------------------|
| name        | The name of the attribute                                | A string                                                                         |
| type        | The type of the attribute                                | One of `string`, `number`, `boolean`, `object`, `array`, or a free format string |
