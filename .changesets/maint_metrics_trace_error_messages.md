### Trace and metrics exporter wrappers to append details to errors ([PR #8363](https://github.com/apollographql/router/pull/8363))

Error messages raised during tracing and metric exports now indicate if the error occurred when exporting to Apollo Studio or to the user's configured OTLP or Zipkin endpoint. For example, errors that occur when exporting Apollo Studio traces will look like:
`OpenTelemetry trace error occurred: [apollo traces] <etc>`
While errors that occur when exporting traces to a user's configured OTLP endpoint will look like:
`OpenTelemetry trace error occurred: [otlp traces] <etc>`

By [@bonnici](https://github.com/bonnici) in https://github.com/apollographql/router/pull/8363
