### Fix the redact error feature for Studio

If you were using `tracing.apollo.errors.subgraph.all.redact` and set it to `false` it was still readacting the error until this fix.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3137