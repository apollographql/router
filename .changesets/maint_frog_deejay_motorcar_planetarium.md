### Add OTLP exporter for Apollo metrics ([PR #3354](https://github.com/apollographql/router/pull/3354), [PR #3651](https://github.com/apollographql/router/pull/3651))

This PR adds an OTLP metrics exporter for a Apollo pipeline that can compliment the existing protobuf format.

Note that new metrics of the format `apollo.router.*` are currently not stable.
Once we have added enough metrics to ensure that we are consistent then they will be stabilized and documented.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/3354 and https://github.com/apollographql/router/pull/3651
