# Apollo Router Request Recordings

To record a request:

- enable the recording plugin in the router:

       ```yaml
       plugins:
        experimental.record:
          enabled: true
          storage_path: /tmp/recordings/
       ```

- add `x-apollo-router-record: 1` as a request header:

      ```sh
      curl http://localhost:4000/ \
        -H 'content-type: application/json' \
        -H 'x-apollo-router-record: 1' \
        -d '{"query": "{ me { id } }"}'
      ```

## Sharing with Apollo

Please include a copy of your router configuration.

## Security considerations

Recordings may capture:

- Authentication tokens in HTTP headers
- Private data in responses

**Do not share recordings publicly without reviewing the recorded data!**

## Replay a recording

Inside the [Apollo Router codebase](https://www.github.com/apollographql/router):

```sh
RECORDING_FILE=/tmp/recordings/Query-1698253358.json \
  cargo test --package apollo-router --lib \
  -- plugins::record_replay::runner::tests::replay_recording --exact --nocapture
```
