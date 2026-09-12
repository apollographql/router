### Add `apollo.router.operations.subscriptions.events_sent` histogram metric

Apollo Router now records `apollo.router.operations.subscriptions.events_sent` once when each client subscription ends. This histogram counts payloads written to the client's multipart response stream, including error-only termination notices. Its `reason` attribute lets you compare event counts by termination reason.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/10214
