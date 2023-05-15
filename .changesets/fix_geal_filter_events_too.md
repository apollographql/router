### Filter events if the trace is not sampled 

There's no need to record the events in opentelemetry if the trace is not sampled, the subscriber would have no spans to attach them to

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2999