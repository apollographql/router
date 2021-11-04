# Nodejs subservices demonstration

This configuration allows you to quickly start a router, and nodejs subservices.

## Prerequisites:

- Nodejs: Follow the [install instructions](https://nodejs.org/en/download/) to install nodejs and npm
- Rust: Head over to [the rustup website](https://rustup.rs/) to install rust

## Environment setup

⚠️ Make sure your submodules are up to date if you want to experiment around the local examples!

```sh
$ git submodule update --init --recursive
```

This project will need several available ports on your machine:

- 4001 to 4004: nodejs GraphQL subservices.
- 4000: The Apollo federation router.

### Setup and run the subservices

Let's go to the federation-demo directory, where the nodejs subservices sources are:

```sh
cd examples/nodejs/federation-demo
```

We can now install the nodejs dependencies by running:

```sh
npm install
```

And finally we can run the subservices:

```sh
npm run start-services
```

This command will run all of the microservices at once, respectively:

- [Accounts: http://localhost:4001](http://localhost:4001)
- [Reviews: http://localhost:4002](http://localhost:4002)
- [Products: http://localhost:4003](http://localhost:4003)
- [Inventory: http://localhost:4004](http://localhost:4004)

The output's last lines should look as follows:

```
[start-service-products] 🚀 Server ready at http://localhost:4003/
[start-service-accounts] 🚀 Server ready at http://localhost:4001/
[start-service-reviews] 🚀 Server ready at http://localhost:4002/
[start-service-inventory] 🚀 Server ready at http://localhost:4004/
```

## Running the Apollo federation router

In another terminal window, run the Apollo router by running this command in the project's root directory:

```sh
cargo run -- -p ./examples/nodejs
```

Here is the expected output:

```sh
   Compiling router-bridge v0.1.0 (https://github.com/apollographql/federation.git)
   Compiling apollo-router-core v0.1.0-prealpha.3 (/Users/ignition/projects/apollo/router/crates/apollo-router-core)
   Compiling apollo-router v0.1.0-prealpha.3 (/Users/ignition/projects/apollo/router/crates/apollo-router)
    Finished dev [unoptimized + debuginfo] target(s) in 5.38s
     Running `target/debug/router -p ./examples/nodejs`
Nov 02 17:08:09.926  INFO router: Starting Apollo Router
Nov 02 17:08:10.279  INFO router: Listening on http://127.0.0.1:4000 🚀
```

Go to http://127.0.0.1:4000 to open the [Apollo studio explorer](https://www.apollographql.com/docs/studio/explorer/). Inspect the graph, and run your first queries using the Apollo federation router!
