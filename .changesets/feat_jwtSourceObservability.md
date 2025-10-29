### Improve jwt authorization observability, exposing the source of the jwt ([Issue #TSH-19130])

Router supports having multiple sources where JWT's can be processed (ie: several headers and cookies), 
however the observability from JWT processing does not expose exactly which source was used, 
which is important to be able to detect anomalies or regressions that affect a particular JWT source.

This MR simply adds this important context to the logs, metrics and traces to improve the ability to quickly pinpoint issues.

By [JonChristiansen](https://github.com/theJC) in https://github.com/apollographql/router/pull/8370