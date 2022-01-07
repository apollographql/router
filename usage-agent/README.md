# usage_agent
Temporary home until I decide where to put it

## Authentication

The server must be authenticated to submit messages to the Apollo ingress.
Set the X_API_KEY environment variable to a valid key. Your key will look
something like this:

```
X_API_KEY=service:<graph name>:<secret key>
```

## Design

The usage agent provides a mechanism to relay usage data from an executing
router (or any other graph processing component) to the Apollo data
ingress.

The main design priniciple is to leverage work that's already being done
(namely collecting opentelemetry statistics) to derive statistics for
use in Apollo studio.

### Components

#### ApolloTelemetry

An open telemetry collector which processes spans and extracts data to
create studio "Reports" which are then submited over gRPC to either an
in-process or an out of process usage agent.

#### UsageAgent

A gRPC server which accepts "Reports" and regularly (every 5 seconds)
submits the collected Reports to the Apollo Reporting ingress.

The agent can be configuration to work either in our out of process. If
in-process, this is more convenient, but less robust. If out of process,
then a single agent can be relaying statistics for multiple clients
(routers).
