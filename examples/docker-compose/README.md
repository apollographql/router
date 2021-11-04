# docker-compose supergraph demonstration

This configuration allows you to quickly start a router, with a demo docker-compose.yml file.

## Prerequisites:

- Docker: Follow the [get started](https://www.docker.com/get-started) guide to install docker and docker-compose
- Rust: Head over to [the rustup website](https://rustup.rs/) to install rust

## Environment setup

⚠️ Make sure your submodules are up to date if you want to experiment around the local examples!

```sh
$ git submodule update --init --recursive
```

This project will need several available ports on your machine:

- 4001 to 4004: nodejs subservices exposing functionality the apollo gateway and the Apollo Federation router will expose.
- 6831, 6832, 16686 and 14268: a [Jaeger tracing](https://www.jaegertracing.io/) node that will collect logs and spans from the gateway, the Apollo federation router, and the subservices. The traces are available at http://localhost:16686
- 4000: The Apollo federation router

In this directory, run `docker-compose up -d`:

```bash
ignition@ignition-apollo router % docker-compose up -d
[+] Running 2/0
 ⠿ Container router-jaeger-1    Running                                       0.0s
 ⠿ Container router-services-1  Running                                       0.0s
```

You should be good to go!

## Running the Apollo federation router

In this project's root directory, you can run the following command to build and run the Apollo federation router:

```bash
ignition@ignition-apollo router % cargo run -- -p ./examples/docker-compose
   Compiling router-bridge v0.1.0 (https://github.com/apollographql/federation.git)
   Compiling apollo-router-core v0.1.0-prealpha.3 (/Users/ignition/projects/apollo/router/crates/apollo-router-core)
   Compiling apollo-router v0.1.0-prealpha.3 (/Users/ignition/projects/apollo/router/crates/apollo-router)
    Finished dev [unoptimized + debuginfo] target(s) in 5.38s
     Running `target/debug/router -p ./examples/docker-compose`
Nov 02 17:08:09.926  INFO router: Starting Apollo Router
Nov 02 17:08:10.279  INFO router: Listening on http://127.0.0.1:4000 🚀
```

Go to http://127.0.0.1:4000 to open the [Apollo studio explorer](https://www.apollographql.com/docs/studio/explorer/) and inspect the graph, and run your first queries using the Apollo federation router!
