### Add OTLP exporter for Apollo metrics ([PR #3354](https://github.com/apollographql/router/pull/3354))

This PR adds an OTLP metrics exporter for a future Apollo pipeline that can compliment the existing protobuf format.

It adds:
* many new metrics, although these won't be visible to users just yet.
* metrics around what was configured in the config file.
* filtering for existing and new metrics pipelines.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/3354