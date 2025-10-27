### Add export destination details to trace and metrics error messages ([PR #8363](https://github.com/apollographql/router/pull/8363))

Error messages raised during tracing and metric exports now indicate whether the error occurred when exporting to Apollo Studio or to your configured OTLP or Zipkin endpoint. For example, errors that occur when exporting Apollo Studio traces look like:
`OpenTelemetry trace error occurred: [apollo traces] <etc>`
while errors that occur when exporting traces to your configured OTLP endpoint look like:
`OpenTelemetry trace error occurred: [otlp traces] <etc>`

By [@bonnici](https://github.com/bonnici) in https://github.com/apollographql/router/pull/8363
