### Enables reporting for client libraries that send the library name and version information in operation requests. ([PR #7264](https://github.com/apollographql/router/pull/7264))

Apollo client libraries can send the library name and version information in the `extensions` key of an operation request. If those values are found in a request the router will include them in the telemetry operation report.

By [@calvincestari](https://github.com/calvincestari) in https://github.com/apollographql/router/pull/7264
