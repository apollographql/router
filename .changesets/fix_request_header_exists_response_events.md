### Allow `exists` conditions with `request_header` selectors on response-stage coprocessor and event configurations ([PR #8964](https://github.com/apollographql/router/pull/8964))

Using `exists: { request_header: <name> }` as a condition on response-stage coprocessor or telemetry event configurations (e.g. `on: response`) previously caused the router to reject the configuration at startup with a validation error, even though the condition is valid and works correctly at runtime.

The validator was incorrectly rejecting request-stage selectors inside `Exists` conditions for response-stage configurations. This is safe because `evaluate_request()` pre-resolves these conditions before they are stored for response-time evaluation: if the header is present the condition becomes `True`; if absent, the event or coprocessor call is discarded and never reaches the response stage.

By [@OriginLeon](https://github.com/OriginLeon) in https://github.com/apollographql/router/pull/8964
