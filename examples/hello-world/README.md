# quickstart supergraph demonstration

This configuration allows you to quickly start a router, connected to subgraphs exposed in apollo studio.

## Prerequisites:

- None! We will use a tarball available [in the Github releases section](https://github.com/apollographql/router/releases)

If you would however rather compile the binary yourself, you will need:

- Rust: Head over to [the rustup website](https://rustup.rs/) to install rust

## Environment setup

This project will need only one port on your machine:

- 4000: The Apollo federation router

If you would like to change the server's used port, change the `examples/hello-world/configuration.yaml` file's `listen` entry:

```yml
listen: 127.0.0.1:<YOUR_PORT>
```

You should be good to go!

## Runnig from the tarball

Download and extract the router release:

On linux:

```sh
curl -o router.tar.gz https://github.com/apollographql/router/releases/download/v0.1.0-prealpha.3/router-0.1.0-prealpha.3-x86_64-linux.tar.gz

tar -xczf router.tar.gz
```

The router release embeds the hello-world example in the examples/hello-world directory:

Run from the router release:

```sh
cd router
./router -p ./examples/hello-world
```

## Building and running the Apollo federation router

In this project's root directory, you can run the following command to build and run the Apollo federation router:

```bash
ignition@ignition-apollo router % cargo run -- -p ./examples/hello-world
   Compiling router-bridge v0.1.0 (https://github.com/apollographql/federation.git)
   Compiling apollo-router-core v0.1.0-prealpha.3 (/Users/ignition/projects/apollo/router/crates/apollo-router-core)
   Compiling apollo-router v0.1.0-prealpha.3 (/Users/ignition/projects/apollo/router/crates/apollo-router)
    Finished dev [unoptimized + debuginfo] target(s) in 5.38s
     Running `target/debug/router -p ./examples/hello-world`
Nov 02 17:08:09.926  INFO router: Starting Apollo Router
Nov 02 17:08:10.279  INFO router: Listening on http://127.0.0.1:4000 🚀
```

Go to http://127.0.0.1:4000 to open the [Apollo studio explorer](https://www.apollographql.com/docs/studio/explorer/) and inspect the graph, and run your first queries using the Apollo federation router!
