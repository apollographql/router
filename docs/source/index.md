---
title: The Apollo router
sidebar_title: Overview
description: guide to using Apollo router
---

The router exposes a [supergraph](https://www.apollographql.com/docs/federation/) generated from one or more federation enabled subgraphs. It receives a GraphQL query, transforms it into a series of subgraph queries, aggregates the response then sends it to the client.

## Setup

First, let's install the router:

```
cargo install apollo-router
```

## Usage

Apollo Federation Router requires `configuration.yaml` and `supergraph.graphql`
to be supplied. These are either located in the current directory or explicitly
specified via flag, either by an absolute path, or a path relative to the current
directory.

```
OPTIONS:
    -c, --config <configuration-path>    Configuration file location [env:
                                         CONFIGURATION_PATH=]
    -s, --schema <schema-path>           Schema location [env: SCHEMA_PATH=]
```

### Configuration file

The configuration file is in YAML format. Here is a reduced list of options, see [configuration](../configuration) for the complete list:

```yaml
# Configuration options pertaining to the http server component
server:
  # The socket address and port to listen on
  # Defaults to 127.0.0.1:4000
  listen: 127.0.0.1:4000
# Mapping of name to subgraph that the router may contact
subgraphs:
  # Configuration for a subgraph
  accounts:
    # url of the subgraph
    routing_url: http://localhost:4001/graphql
  products:
    routing_url: http://localhost:4003/graphql
# OpenTelemetry configuration. Choose either jaeger or otlp
opentelemetry:
  # Configuration to send traces and metrics to a Jaeger service
  jaeger:
```

### Generating the supergraph

The `schema` argument is the supergraph that will be exposed by the router, it is not created automatically by the router from the subgraphs.

To learn how to compose your supergraph schema with the Rover CLI, see the [Federation quickstart](https://www.apollographql.com/docs/federation/quickstart/#3-compose-the-supergraph-schema).

> 🚧 For now, the router requires the supergraph to be generated separately. In the future, it might be possible to start a router with the right schema directly from Rover

## Launching the router

Once the configuration file and supergraph are written, start the router:

```
$ apollo-router -c configuration.yaml --schema supergraph.graphql
Oct 20 12:11:05.128  INFO router: Starting Apollo Federation
Oct 20 12:11:05.581  INFO router: Listening on http://127.0.0.1:4000 🚀
```
