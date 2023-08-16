# Multipart subscriptions protocol

Instead of relying on WebSockets, the subscriptions protocol supported by the router uses streaming multipart HTTP responses, following the lead of the [Incremental Delivery over HTTP](https://github.com/graphql/graphql-over-http/blob/main/rfcs/IncrementalDelivery.md) spec that is already in use to support `@defer` today.

## Communication

When sending a request containing a subscription to the router, clients should include the following `Accept` header to indicate their support for the multipart subscriptions protocol:
```
Accept: multipart/mixed; boundary="graphql"; subscriptionSpec="1.0", application/json
```

> Note that `boundary` should always be `graphql` for now, and `subscriptionSpec` is `1.0` for the current version of the protocol.

The router will then respond with a stream of body parts, following the [definition of multipart content specified in RFC1341](https://www.w3.org/Protocols/rfc1341/7_2_Multipart.html).

An example response might look as follows:

```
--graphql
Content-Type: application/json

{}
--graphql
Content-Type: application/json

{"payload": {"data": { "newPost": { "id": 123, "title": "Hello!"}}}}
--graphql--
```

When HTTP/1 is used, the response will use `Transfer-Encoding: chunked`, but this is not needed for HTTP/2 (which has built-in support for data streaming) and actually [disallowed](https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Transfer-Encoding).

## Heartbeats

The router will send periodic heartbeats to avoid any intermediaries from closing the connection. Heartbeats are sent as an empty JSON object without a payload, and they should be silently ignored by clients:

```
--graphql
Content-Type: application/json

{}
--graphql--
```

## Messages

The protocol differentiates between transport-level concerns and the GraphQL response payloads themselves. One reason for this is that [the response format is part of the GraphQL spec](https://spec.graphql.org/draft/#sec-Response-Format), and additional fields might be confusing or could even break client typing.

Except for [heartbeats](#heartbeats), every message will therefore include a `payload`, but the payload may be `null`.

Some errors are part of an execution result. Results may include partial data, and these errors are not fatal (meaning the subscription stream should be kept open). They are therefore delivered within the payload:

```json
{
  "payload": {
    "errors": [...],
    "data": {...},
    "extensions": {...}
  }
}
```

When the router encounters an error that is fatal and should lead to termination of the subscription, it will instead send a message with a top-level `errors` field, and then close the connection:

```json
{
  "payload": null,
  "errors": [...]
}
```

Both types of `errors` will follow the [GraphQL error format](http://spec.graphql.org/draft/#sec-Errors.Error-Result-Format) (but top-level `errors` will never have `locations` or `path`).

