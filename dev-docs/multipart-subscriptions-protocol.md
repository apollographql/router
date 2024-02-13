# Multipart subscriptions protocol

Instead of relying on WebSockets, the subscriptions protocol supported by the router uses streaming multipart HTTP responses, following the lead of the [Incremental Delivery over HTTP](https://github.com/graphql/graphql-over-http/blob/main/rfcs/IncrementalDelivery.md) spec that is already in use to support `@defer` today.

## Communication

When sending a request containing a subscription to the router, clients MUST support the `multipart/mixed;subscriptionSpec="1.0"` content-type in addition to the `application/json` content-type:

```
Accept: multipart/mixed;subscriptionSpec="1.0", application/json
```

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

> Note: because the parts are always JSON, it is never possible for `\r\n--graphql` to appear in the contents of a part. For convenience, servers MAY use `graphql` as a boundary.
> Clients MUST accomodate any boundary returned by the server in `Content-Type`.

When HTTP/1 is used, the response will use `Transfer-Encoding: chunked`, but this is not needed for HTTP/2 (which has built-in support for data streaming) and actually [disallowed](https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Transfer-Encoding).

## Heartbeats

The router will send periodic heartbeats to avoid any intermediaries from closing the connection. Heartbeats are sent as an empty JSON object without a payload, and they should be silently ignored by clients:

```
--graphql
Content-Type: application/json

{}
--graphql--
```

## GraphQL Responses

When a GraphQL response is received, it is returned in the `payload` field using the [format from the GraphQL spec](https://spec.graphql.org/draft/#sec-Response-Format).

A GraphQL response may contains [errors](https://spec.graphql.org/October2021/#sec-Errors). These errors are not fatal (meaning the subscription stream should be kept open) and are delivered within the payload:

```json
{
  "payload": {
    "errors": [...],
    "data": {...},
    "extensions": {...}
  }
}
```

## Fatal Errors

When the router encounters an error that is fatal and should lead to termination of the subscription, `payload` is null:

```json
{
  "payload": null,
  "errors": [...]
}
```

The top-level `errors` field may be present to give more details about the errors. It follows the [GraphQL error format](http://spec.graphql.org/draft/#sec-Errors.Error-Result-Format) but will never contain `locations` or `path` fields.
After a fatal error, the router closes the connection.

