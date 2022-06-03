# Server
Server implementation is in this crate.

Supports:
* graceful shutdown
* hot reloading config
* hot reloading schema

## Components
* FederatedServer - The main entry point. Can be configured with config, schema and a shutdown hook.
* StateMachine - Responds to a stream of events to startup, hot reload, or shutdown.  
* HttpServerFactory - Interface for creating an http implementation.
* HyperHttpServerFactory - An implementation of HttpServerFactory that uses Hyper.

# Example execution
![Server sequence diagram](./images/sequence.svg)
1. Configuration, Schema and Shutdown event streams.
1. Combine streams to one unified stream for feeding to the state machine.
1. State machine processes events until drained.
1. State machine may start/restart/stop http server.
1. Result of the state machine supplied back to the caller.

