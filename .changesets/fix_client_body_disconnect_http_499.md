### Report client body disconnects as HTTP 499 ([Issue #9634](https://github.com/apollographql/router/issues/9634))

When a client disconnects while Apollo Router is still reading the incoming request body, the resulting body-read error previously reached the HTTP boundary as an internal service error and was reported as HTTP 500.

Disconnects that occur specifically while reading the inbound client body are now classified as HTTP 499, matching Router's existing "request canceled by client" behavior. Detection happens at known inbound request-body read sites, and those failures are wrapped in a typed `ClientRequestBodyReadError` that the HTTP boundary maps to 499.

This distinction is important because the Router service stack can also contain outbound network errors. A `ConnectionReset` from a coprocessor or another backend remains a server-side failure rather than being mistaken for a client cancellation.

By [@trippyogi](https://github.com/trippyogi) in https://github.com/apollographql/router/pull/10010
