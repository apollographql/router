# External Subgraph nodejs example

This is an example that involves a nodejs coprocessor alongside a router.

## Usage

- Start the coprocessor:

```bash
$ npm install && npm run start
```

- Start the router 
```
$ APOLLO_KEY="YOUR_APOLLO_KEY" APOLLO_GRAPH_REF="YOUR_APOLLO_GRAPH_REF" cargo run -- --configuration router.yaml
```
