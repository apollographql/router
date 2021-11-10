# Apollo Router

The **Apollo Router** is a configurable, high-performance **graph router** for a [federated graph](https://www.apollographql.com/docs/federation/):

## Getting started

Follow the [quickstart tutorial](https://www.apollographql.com/docs/router/quickstart/) to get up and running with the Apollo Router.

## Status

🚧 Apollo Router is experimental software, not yet ready for production use.
It is not yet feature complete nor fully compliant to the GraphQL specification, we are
working on it.
It can already perform queries though, so we'd welcome experiments and feedback on it.

## Usage

Apollo Router requires [a supergraph file](https://www.apollographql.com/docs/rover/supergraphs/) to be passed as the `--supergraph` argument and [optional configuration](https://www.apollographql.com/docs/router/configuration/).
to be supplied. These are either located in the current directory or explicitly
specified via flag, either by an absolute path, or a path relative to the current
directory.

```
OPTIONS:
    -c, --config <configuration-path>    Configuration file location [env:
                                         CONFIGURATION_PATH=]
    -s, --supergraph <supergraph-path>   Supergraph Schema location [env: SUPERGRAPH_PATH=]
```

This CLI is not meant to be a long term thing, as users will likely use Rover
to start the server in future.

## Project maintainers

Apollo Graph, Inc.
