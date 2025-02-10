### Reduce demand control allocations on start/reload ([PR #6754](https://github.com/apollographql/router/pull/6754))

When enabled, preallocates capacity for demand control's processed schema and shrinks to fit after processing. When disabled, skips the type processing entirely to minimize startup impact.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/6754
