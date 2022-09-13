# Apollo Telemetry
Transfer statistics (e.g., operation usage) to Apollo Studio's ingress

## Authentication

The server must be authenticated to submit messages to the Apollo ingress.
Furthermore, the statistics must be submitted "to" a particular graph. In
the existing products this is accomplished using [environment variables](https://www.apollographql.com/docs/federation/managed-federation/setup/#4-connect-the-gateway-to-studio).

In the router, we have a configuration file which can be dynamically
re-loaded, so it makes more sense to include this configuration here. There
is a new optional section that looks like this:

```
telemetry:
  # Optional Apollo telemetry configuration.
  apollo: 
  
    # Optional external Spaceport URL.
    # If not specified an in-process spaceport is used. 
    endpoint: "https://my-spaceport"
  
    # Optional Apollo key. If not specified the env variable APOLLO_KEY will be used.
    apollo_key: "${APOLLO_KEY}"
    
    # Optional graphs reference. If not specified the env variable APOLLO_GRAPH_REF will be used.   
    apollo_graph_ref: "${APOLLO_GRAPH_REF}"
```

## Design

There are two main components:
 - Apollo Telemetry
 - Apollo Spaceport

### Configuration

The telemetry statistics are internally delivered via gRPC service to a spaceport
which then buffers data before finally delivering statistics to the Apollo
ingress. That spaceport can be internal, which is the default, or external.

The spaceport is configured from a new optional configuration section which looks
like this:

```
telemetry:
  # Optional Apollo telemetry configuration.
  apollo: 
  
    # Optional external Spaceport URL.
    # If not specified an in-process spaceport is used. 
    endpoint: "https://my-spaceport"
```

### Components

#### ApolloTelemetry

An open telemetry collector which processes spans and extracts data to
create "Reports" which are then submited over gRPC to either an
in-process or an out of process spaceport.

#### Spaceport

A gRPC server which accepts "Reports" and regularly (every 5 seconds)
submits the collected Reports to the Apollo Reporting ingress. If the
quantity of Reports exceeds a specified limit, then a transfer will
be triggered early, so a very busy Spaceport will deliver more frequently
than every 5 seconds.

Delivery to the ingress is on a "best efforts" basis and the spaceport
will attempt to deliver the data 5 times before discarding. 

