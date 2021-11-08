---
title: Apollo Router
sidebar_title: Overview
description: Overview
---

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

The Apollo Router is implemented in Rust, which provides [dramatic speed and bandwidth benefits](./) over the `@apollo/gateway` extension of Apollo Server.

[Get started!](./quickstart/hosted)
