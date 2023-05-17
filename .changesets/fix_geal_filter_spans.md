### Filter spans before sending them to the opentelemetry layer 

the sampling configuration in the opentelemetry layer only applies when the span closes, so in the meantime a lot of data is created just to be dropped. This adds a filter than can sample spans before the opentelemetry layer. The sampling decision is done at the root span, and then derived from the parent span in the rest of the trace.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2894