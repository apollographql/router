### Add OTLP exporter for Apollo metrics ([PR #3354](https://github.com/apollographql/router/pull/3354))

This PR adds an OTLP metrics exporter for a future Apollo pipeline that can compliment the existing protobuf format. It is currently disabled by default.

Note that new metrics of the format `apollo.router.*` are currently not stable.
Once we have added enough metrics to ensure that we are consistent then they will be stabilized and documented.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/3354
