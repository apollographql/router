# Record/Replay

The Record plugin captures aspects of the entire request pipeline

- client request (query, variables, operation name, headers)
- client response (data, errors, extensions, headers)
- query plan
- subgraph requests (query, variables, operation name, headers)
- subgraph responses (data, errors, extensions, headers)

To enable recording:

```yaml
plugins:
  experimental.record:
    enabled: true
    storage_path: /some/directory
```

And add a header to the request:

```
curl http://localhost:4000/ \
   -H 'content-type: application/json' \
   -H 'x-apollo-router-record: 1' \
   -d '{"query": "{hello}"}'
```

The Router writes JSON files to disk that you can use for replay.

## Replay

You can quickly replay a recording with an existing test:

```sh
RECORDING_FILE=/tmp/recordings/Query-1698253358.json \
  cargo test --package apollo-router --lib \
  -- plugins::record_replay::replay::tests::replay_recording --exact --nocapture
```
