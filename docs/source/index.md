---
title: Apollo Router
sidebar_title: 🔭 Overview
description: Overview
---

> ⚠️ **The Apollo Router is in public pre-alpha.** Until it is generally available, do not use it in business-critical graphs. [Learn about release stages.](https://www.apollographql.com/docs/resources/release-stages/#open-source-release-stages)

The **Apollo Router** is a configurable, high-performance **graph router** for a [federated graph](https://www.apollographql.com/docs/federation/):

```mermaid
flowchart BT;
  clients(Clients);
  subgraph " ";
  gateway(["Apollo Router<br/>(replaces @apollo/gateway)"]);
  serviceA[Users<br/>subgraph];
  serviceB[Products<br/>subgraph];
  serviceC[Reviews<br/>subgraph];
  gateway --- serviceA & serviceB & serviceC;
  end;
  clients -.- gateway;
  class clients secondary;
```

The Apollo Router is [implemented in Rust](https://github.com/apollographql/router), which provides [performance benefits](https://www.apollographql.com/blog/announcement/backend/apollo-router-our-graphql-federation-runtime-in-rust/) over the TypeScript Apollo Gateway.

[Try it out!](./quickstart/)
