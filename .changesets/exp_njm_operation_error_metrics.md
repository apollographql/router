### Experimental per-operation error metrics ([PR #6443](https://github.com/apollographql/router/pull/6443))

Adds a new experimental OpenTelemetry metric that includes error counts at a per-operation and per-client level. These metrics contain the following attributes:
* Operation name
* Operation type (query/mutation/subscription)
* Apollo operation ID
* Client name
* Client version
* Error code

This metric is currently only sent to GraphOS and is not available in 3rd-party OTel destinations.

By [@bonnici](https://github.com/bonnici) in https://github.com/apollographql/router/pull/6443
