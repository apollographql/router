# Apollo Telemetry
Relay statistics from router to apollo studio ingress.

## Authentication

The server must be authenticated to submit messages to the Apollo ingress.
Furthermore, the statistics must be submitted "to" a particular graph. In
the existing products this is accomplished using [environment variables](https://www.apollographql.com/docs/federation/managed-federation/setup/#4-connect-the-gateway-to-studio).

In the router, we have a configuration file which can be dynamically
re-loaded, so it makes more sense to include this configuration here. There
is a new optional section that looks like this:

```
graph:
  key: <YOUR_GRAPH_API_KEY>
  reference: <YOUR_GRAPH_ID>@<VARIANT>
```

## Design

There are two main components:
 - Apollo Telemetry
 - Relay

### Configuration

The telemetry statistics are internally delivered via gRPC service to a relay
which then buffers data before finally delivering statistics to the Apollo
ingress. That relay can be internal, which is the default, or external.

The relay is configured from a new optional configuration section which looks
like this:

```
studio:
  external_relay: false
  collector: https://127.0.0.1:50051
  listener: 0.0.0.0:50051
```

(The above values are the defaults, so configuring like this will have the same
results as performing no configuration.)

### external_relay

This directs the router to start an internal relay (default: false) or to send
statistics to an externally configured relay.

### collector

This directs the router to send statistics to this configured URL.

### listener

This is only used if external relay is false, in which case a listening relay
is spawned and will listen at the specified address.

### Components

#### ApolloTelemetry

An open telemetry collector which processes spans and extracts data to
create studio "Reports" which are then submited over gRPC to either an
in-process or an out of process relay

#### Relay

A gRPC server which accepts "Reports" and regularly (every 5 seconds)
submits the collected Reports to the Apollo Reporting ingress. If the
quantity of Reports exceeds a specified limit, then a transfer will
be triggered early, so a very busy Relay will deliver more frequently
than every 5 seconds.

Delivery to the ingress is on a "best efforts" basis and the relay
will attempt to deliver the data 5 times before discarding. 

