### Add ability to transmit un-redacted errors from federated traces to Apollo Studio

When using subgraphs which are enabled with [Apollo Federated Tracing](https://www.apollographql.com/docs/router/configuration/apollo-telemetry/#enabling-field-level-instrumentation), the error messages within those traces will be **redacted by default**.

New configuration (`tracing.apollo.errors.subgraph.all.redact`, which defaults to `true`) enables and disables the redaction mechanism.  Similar configuration (`tracing.apollo.errors.subgraph.all.send`, which also defaults to `true`) enables and the entire transmission of the error to Studio.

The error messages returned to the clients are **not** changed or redacted from their previous behavior.

To enable sending subgraph's federated trace error messages to Studio **without redaction**, you can set the following configuration:

```yaml title="router.yaml"
telemetry:
  apollo:
    errors:
      subgraph:
        all:
          send: true # (true = Send to Studio, false = Do not send; default: true)
          redact: false # (true = Redact full error message, false = Do not redact; default: true)
```

It is also possible to configure this **per-subgraph** using a `subgraphs` map at the same level as `all` in the configuration, much like other sections of the configuration which have subgraph-specific capabilities:

```yaml title="router.yaml"
telemetry:
  apollo:
    errors:
      subgraph:
        all:
          send: true
          redact: false # Disable redaction as a default.  The `accounts` service enables it below.
        subgraphs:
          accounts: # Applies to the `accounts` subgraph, overriding the `all` global setting.
            redact: true # Redact messages from the `accounts` service.
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3011