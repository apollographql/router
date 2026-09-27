### Add the `->encode` and `->decode` mapping methods for connectors

Connector mappings can now encode and decode strings with a named codec, so APIs that exchange base64 payloads no longer need a coprocessor or a wrapper service. The Gmail API, for example, returns message bodies as unpadded base64url and expects a base64url-encoded message when sending one:

```graphql
@connect(
  source: "gmail"
  http: { GET: "/users/me/messages/{$args.id}" }
  selection: """
  id
  html: payload.body.data->decode("base64url")
  """
)
```

Three codecs are supported:

- `"base64"` encodes with the standard alphabet (RFC 4648 §4), padded.
- `"base64url"` encodes with the URL-safe alphabet (RFC 4648 §5), unpadded.
- `"json"` behaves exactly like `->jsonStringify` (encode) and `->jsonParse` (decode).

JSON has no byte type, so the base64 codecs work on text: `->encode` requires a string and encodes its UTF-8 bytes, and `->decode` returns a string. Decoding is lenient about the variant, accepting either alphabet with or without padding, but it reports an error instead of returning a garbled string when the input is not valid base64 or the decoded bytes are not valid UTF-8. In request URLs and bodies, a literal codec name is checked when the schema is validated, so a typo like `->encode("base46")` fails composition instead of every request.

By [@fernando-apollo](https://github.com/fernando-apollo) in https://github.com/apollographql/router/pull/10282
