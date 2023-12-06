# Apollo Router Request Recordings

The files in this directory contain recordings of GraphQL requests and responses to Apollo Router. They may contain private or sensitive data, so please review before sharing.

## Sharing with Apollo

Please include a copy of your router configuration for accurate reproductions.

**Do not share recordings publicly without reviewing the recorded data!**

## Security considerations

Recordings may capture:

- Authentication tokens in HTTP headers
- Private data in request operations or variables
- Private data in responses

## Replay a recording

Inside the [Apollo Router codebase](https://www.github.com/apollographql/router):

```sh
RECORDING_FILE=/tmp/recordings/Query-1698253358.json \
  cargo test --package apollo-router --lib \
  -- plugins::record_replay::replay::tests::replay_recording --exact --nocapture
```
