<a href="#"><img src="https://raw.githubusercontent.com/apollographql/space-kit/main/src/illustrations/svgs/satellite1.svg" width="100%" height="144"></a>

[![CircleCI](https://circleci.com/gh/apollographql/router/tree/main.svg?style=shield)](https://circleci.com/gh/apollographql/router/tree/main)
[![Netlify Status](https://api.netlify.com/api/v1/badges/29a5691a-77f6-4253-99a4-45027cfa3278/deploy-status)](https://app.netlify.com/sites/apollo-router-docs/deploys)

# Apollo Router

The **Apollo Router** is a configurable, high-performance **graph router** for a [federated graph](https://www.apollographql.com/docs/federation/).

## Getting started

Follow the [quickstart tutorial](https://www.apollographql.com/docs/router/quickstart/) to get up and running with the Apollo Router.

See [the documentation](https://www.apollographql.com/docs/router) for more details.

## Status

🚧 Apollo Router is experimental software.  We're working on it!  See our [release stages](https://www.apollographql.com/docs/resources/release-stages/) for more information.

The Apollo Router can serve queries but is not yet feature complete nor fully compliant with the GraphQL specification.

We'd encourage you to experiment with it, report troubles and offer your feedback on it!

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

## Who is Apollo?

[Apollo](https://apollographql.com/) is building software and a graph platform to unify GraphQL across your apps and services. We help you ship faster with:

* [Apollo Studio](https://www.apollographql.com/studio/develop/) – A free, end-to-end platform for managing your GraphQL lifecycle. Track your GraphQL schemas in a hosted registry to create a source of truth for everything in your graph. Studio provides an IDE (Apollo Explorer) so you can explore data, collaborate on queries, observe usage, and safely make schema changes.
* [Apollo Federation](https://www.apollographql.com/apollo-federation) – The industry-standard open architecture for building a distributed graph.  Compose and manage your graphs using [Rover](https://www.apollographql.com/rover/) and then use Apollo Router to query plan and route requests across multiple subgraphs.
* [Apollo Client](https://www.apollographql.com/apollo-client/) – The most popular GraphQL client for the web. Apollo also builds and maintains [Apollo iOS](https://github.com/apollographql/apollo-ios) and [Apollo Android](https://github.com/apollographql/apollo-android).
* [Apollo Server](https://www.apollographql.com/docs/apollo-server/) – A production-ready JavaScript GraphQL server that connects to any microservice, API, or database. Compatible with all popular JavaScript frameworks and deployable in serverless environments.

## Learn how to build with Apollo

Check out the [Odyssey](https://odyssey.apollographql.com/) learning platform, the perfect place to start your GraphQL journey with videos and interactive code challenges. Join the [Apollo Community](https://community.apollographql.com/) to interact with and get technical help from the GraphQL community.

## Project maintainers

Apollo Graph, Inc.

## Licensing

Source code in this repository is covered by the Elastic License 2.0. The
default throughout the repository is a license under the Elastic License 2.0,
unless a file header or a license file in a subdirectory specifies another
license.  [See the LICENSE](./LICENSE) for the full license text.
