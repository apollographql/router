### Add ability to rename metrics ([PR #8424](https://github.com/apollographql/router/pull/8424))

The router can now rename instruments via OpenTelemetry views.

Benefits:
- **Cost optimization**: Some observability platforms only allow tag indexing controls on a per-metric name basis. Using OTLP semantic naming conventions and having the same metric name emitted by different services can prevent effective use of these controls.
- **Convention alignment**: Many customers have specific metric naming conventions across their organization—this feature allows them to align with those conventions. 

By [Jon Christiansen](https://github.com/theJC) in https://github.com/apollographql/router/pull/8412
