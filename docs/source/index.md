---
title: Apollo Router
sidebar_title: Overview
description: Overview
---

The **Apollo Router** is a configurable, high-performance **gateway** for a [federated graph](https://www.apollographql.com/docs/federation/):

```mermaid
flowchart BT;
  clients(Clients);
  subgraph " ";
  gateway(["Gateway<br/>(Apollo Router)"]);
  serviceA[Users<br/>subgraph];
  serviceB[Products<br/>subgraph];
  serviceC[Reviews<br/>subgraph];
  gateway --- serviceA & serviceB & serviceC;
  end;
  clients -.- gateway;
  class clients secondary;
```

The Apollo Router is implemented in Rust, which provides [dramatic speed and bandwidth benefits](./) over other gateway libraries (including the `@apollo/gateway` extension of Apollo Server).

[Get started!](./configuration/)
