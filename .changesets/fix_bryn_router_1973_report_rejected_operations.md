### Report operations rejected by authorization to Apollo Studio ([PR #9911](https://github.com/apollographql/router/pull/9911))

When authorization refuses an operation outright, Apollo Studio now receives it as an operation, identified by the signature of the query the client sent and carrying the client name, version, and request count. Studio previously received the operation count alone and had nothing to attribute it to.

A refused operation counts as one licensed operation.

The `Authorization error` log event for a refused operation now appears under the `execution` span instead of inside `query_planning`. Update log or trace filters that match this event by span name.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/9911
