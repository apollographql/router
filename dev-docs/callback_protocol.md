# GraphQL subscription over callback protocol

## Communication

The callback protocol for GraphQL subscription aims to be an alternative to existing websocket protocols when using Apollo Federation and communicate between the Apollo Router and an event source (a subgraph for example).

Main goal is to not keep an opened connection between a subgraph and the Apollo Router in order to be more efficient.

**All** payloads contain the `kind` field outlining the kind of payload it is, in our case it will always be `"subscription"`. The payload also always contains the `action` field describing what kind of action we want to process, the `verifier` (to check that we're authorized to make that callback) the Apollo Router sent via `extensions` in the request and finally the `id` field which is the identifier (an uuid v4) for a specific opened subscription. 

Depending on the `action`, the payload can contain two more _optional_ fields:

- `payload` holding the GraphQL Response when sending the subscription event from the source event to the Apollo Router.
- `errors` used to complete a connection and add errors if critical errors happened. `errors` is an array of GraphQL error.

When opening a GraphQL subscription on the Apollo Router it will directly send a request to the subgraph containing the original subscription and more data related to callback mode in GraphQL `extensions`. For example:

```json
{
    "query": "subscription { userWasCreated { name reviews { body } } }",
    "extensions": {
        "subscription": {
            "callback_url": "http://localhost:4000/callback/c4a9d1b8-dc57-44ab-9e5a-6e6189b2b945",
            "subscription_id": "c4a9d1b8-dc57-44ab-9e5a-6e6189b2b945",
            "verifier": "XXX"
        }
    }
}
```

When a subgraph receives a subscription request. It must first make a `check` request to the callback endpoint (see below) with the given data (`callback_url`, `subscription_id` and a `verifier`). This will ensure the subgraph is able to send source stream events, and that the `subscription_id` and the `verifier` are correct. A successful call to the callback URL with a `check` yields an empty body and a header `subscription-protocol: callback`. Only then can the subgraph can answer the initial subscription request, and start notifying the callback URL with subscription events.


The event source can terminate a subscription any time by sending the `complete` action message (cf message types just below) and include `errors` if needed.


If the subscription is closed or doesn't exist anymore on the Apollo Router then when the source event will send an event message to the callback endpoint it will returns a 404 HTTP status code.
So at the event source side if you're receiving a 404 HTTP status code from the callback endpoint you must terminate the subscription.

## Message types

> Note that all messages are sent for the source event to the Apollo Router

### `check`

Indicates that the event source wants to check that the callback url and subscription id it received is correct. If the subscription id is correct the callback endpoint must respond with a 204 HTTP status code without payload.

> When opening a `subcription` this is the first message to be sent to the callback endpoint and it MUST be synchronous. It means it's called directly when the event source is receiving a request for a subscription before executing it. The event source MUST call the callback endpoint and send this message in order to check if it's able to communicate with the Apollo Router. If it fails it should directly return an error, if it works it returns an empty body with 204 HTTP status code. Once the subscription has been correctly created this message can also be used to heartbeat a single subscription, if you want to heartbeat several subscriptions at once, use the `heartbeat` message. 

```json
{
    "kind": "subscription",
    "action": "check",
    "id": "c4a9d1b8-dc57-44ab-9e5a-6e6189b2b945",
    "verifier": "XXX"
}
```

### `heartbeat`

This message is used to heartbeat the subscription and to check the event source can access the callback endpoint. If one of the subscription ids is incorrect the callback endpoint must respond with a 400 HTTP status code and a payload containing the `invalid_ids` field (it's an array of incorrect ids), the `verifier` to use next time and the `id` linked to that `verifier`. If all ids are correct then the callback endpoint must respond a 204 HTTP status code without payload. The `id` field correspond to the `id` you received from the router with the provided `verifier` you're sending.

> If no IDs are still valid then we will return a 404 error status code without any payload

```json
{
    "kind": "subscription",
    "action": "heartbeat",
    "id": "c4a9d1b8-dc57-44ab-9e5a-6e6189b2b945",
    "ids": ["c4a9d1b8-dc57-44ab-9e5a-6e6189b2b945", "c4a9d1b8-dc57-44ab-9e5a-6e6189b2b254"],
    "verifier": "XXX"
}
```

Example of payload sent with HTTP status code 400 if it contains incorrect ids:

```json
{
    "id": "c4a9d1b8-dc57-44ab-9e5a-6e6189b2b945",
    "invalid_ids": ["c4a9d1b8-dc57-44ab-9e5a-6e6189b2b254"],
    "verifier": "XXX"
}
```

### `next`

Operation execution result(s) from the event source created by subscription. The `payload` field must be a compliant GraphQL execution result. After all results have been emitted, the `complete` message will follow indicating stream completion.

```json
{
    "kind": "subscription",
    "action": "next",
    "payload": {
        "data": {
            "foo": "bar"
        }
    },
    "id": "c4a9d1b8-dc57-44ab-9e5a-6e6189b2b945",
    "verifier": "XXX"
}
```

### `complete`

It indicates that the requested GraphQL subscription execution has completed. If the message contains the `errors` field it means the operation failed, if `errors` is empty it means the operation has been executed successfully. In each cases (`errors` empty or not), when receiving `complete` eventt then the Apollo Router will close the subscription to the client. The `errors` field is optional and is an array of GraphQL errors.

```typescript
{
    "kind": "subscription",
    "action": "complete",
    "errors": [{ // Optional if successful
        "message": "something is wrong"
    }],
    "id": "c4a9d1b8-dc57-44ab-9e5a-6e6189b2b945",
    "verifier": "XXX"
}
```

### Error cases

+ The event source can't call the callback endpoint with the `check` message either because the subscription id is incorrect nor the callback endpoint is available.
+ Any messages sent to the callback endpoint by the source event is falling in error, if it's a 404 HTTP status code it means the subscription doesn't exist anymore and should be closed on the source event side. All other errors are unexpected and should result in a termination of the subscription at the source event level.
+ The source event didn't send the `check` message every 5 secs and so the subscription is automatically closed at the Apollo Router level and will cut the connection with the client.

## Examples

For the sake of clarity, the following examples demonstrate the callback protocol.

### Streaming operation

#### `subscription` operation

1. _The Apollo Router_ receives a `subscription` operation
2. _The Apollo Router_ generates an unique ID (uuid v4) for the following subscription
3. _The Apollo Router_ sends a query containing a [GraphQL request payload](https://github.com/graphql/graphql-over-http/blob/main/spec/GraphQLOverHTTP.md#request-parameters) with all callback data in `extensions`

Example:

```json
{
    "query": "subscription { userWasCreated { name reviews { body } } }",
    "extensions": {
        "subscription": {
            "callback_url": "http://localhost:4000/callback/c4a9d1b8-dc57-44ab-9e5a-6e6189b2b945",
            "subscription_id": "c4a9d1b8-dc57-44ab-9e5a-6e6189b2b945",
            "verifier": "XXX"
        }
    }
}
```

4. _Event source_ receives the GraphQL `subscription` with all callback data directly in `extensions`.
5. _Event source_ calls the callback endpoint given in `extensions` with a [`check`](#check) to init the subscription to _the Apollo Router_.

Payload example for `POST` on `http://localhost:4000/callback/c4a9d1b8-dc57-44ab-9e5a-6e6189b2b945`:

```json
{
    "kind": "subscription",
    "action": "check",
    "id": "c4a9d1b8-dc57-44ab-9e5a-6e6189b2b945",
    "verifier": "XXX"
}
```

6. _Event source_ receives 204 HTTP status code from the call to the callback endpoint
    - If an error happens or you don't receive 204 HTTP status code then directly return a >= 400 HTTP status code to the _Apollo Router_
7. _Event source_ spawns a background task to listen on `subscription` events
    - Every 5 seconds the _Event source_ must call the callback endpoint with a [`heartbeat` payload](#heartbeat) to heartbeat and confirm it's still listening to the subscription
    - Every received events the _Event source_ calls the callback endpoint with a [`next` payload](#next)
    - If an error appears the _Event source_ calls the callback endpoint with a [`complete` payload with errors field](#complete)
    - If the stream of events is done then send a [`complete` payload WITHOUT errors field](#complete)
8. _Event source_ returns empty body containing a new header `subscription-protocol: callback` in answer to the initial call from _Apollo Router_ 