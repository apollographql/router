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

## Design principles

The development of the Apollo Router is driven by those principles that inform
architecture decisions and implementation.
**Correctness:** the router strives to be the most correct implementation of GraphQL and Federation, we care about testing and documenting everything implied by the specification, up to failure cases. The router’s behavior should follow the principle of least surprise for developers.

**Reliability:** the router is a critical part of GraphQL APIs, so it must be one of the strongest parts of the infrastructure. This implies stability in its behavior (no crashes, infinite loops, leaks, etc), in its availability (predictable latency, RAM and CPU usage, scalability) and observability (metrics, alerts). It should give strong confidence to infrastructure people that they can learn its limits and operate it safely.

**Safe experimentation:** the router will support all the future work around Federation, so it must allow new ideas and explorations without disturbing existing features. The project is still in movement, we cannot allow it to crystallize too early, while still following the principles of correctness and reliability.

**Usability:** the router must be simple to operate. Prefer extensibility over configuration options, and ensure that the user has enough information to help themselves when things go wrong. For example:
* Common environmental misconfiguration should detected and surfaced to the user in the form of mitigation steps.
* User supplied extensions should be observable and flagged when they cause performance issues. Tell the users how much time an extension is consuming per request and why.

### Architecture

The design principles are guiding these architecture areas:
**Unit testability:** All new code should be unit testable, or have a good reason why it is not. This may mean spending a little extra time to ensure code is testable in isolation. Do not rely solely on integration testing.

**Integration test suite:** we will integrate with the gateway’s test suite and help improve it to test all aspects of the specifications. In particular, this test suite will verify failure cases like invalid queries or network problems. Integration tests must be bullet proof, and must not fail in the case of slow test execution or race conditions.

**Measurement and learning:** reliability has to be tested and measured, through benchmarks, profiling, and through exploration of the router’s limits. We want to learn how to operate the router and what is its nominal point. To that end, the router shall be instrumented in detail, allowing us to measure how code changes affect it. We especially take care of measuring the overhead of new features, to keep bounded latency and resource usage.

**Extensibility:** by allowing extensions and directives to modify the router’s behavior, we will be able to run experiments, test new features, while limiting its impact to specific queries or endpoints, and always in a way that is easy to deactivate at runtime (feature flags, canaries, etc). This will be especially important to keep development velocity once the router begins running in production.

## Project maintainers

Apollo Graph, Inc.

## Licensing

Source code in this repository is covered by the Elastic License 2.0. The
default throughout the repository is a license under the Elastic License 2.0,
unless a file header or a license file in a subdirectory specifies another
license.  [See the LICENSE](./LICENSE) for the full license text.
