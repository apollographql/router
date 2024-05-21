# Apollo Router project

This generated project is set up to create a custom Apollo Router binary that may include plugins that you have written.

> Note: The Apollo Router is made available under the Elastic License v2.0 (ELv2).
> Read [our licensing page](https://www.apollographql.com/docs/resources/elastic-license-v2-faq/) for more details.

# Compile the router

To create a debug build use the following command.
```bash
cargo build
```
Your debug binary is now located in `target/debug/router`

For production, you will want to create a release build.
```bash
cargo build --release
```
Your release binary is now located in `target/release/router`

# Run the Apollo Router

1. Download the example schema

   ```bash
   curl -sSL https://supergraph.demo.starstuff.dev/ > supergraph-schema.graphql
   ```

2. Run the Apollo Router

   During development it is convenient to use `cargo run` to run the Apollo Router as it will
   ```bash 
   cargo run -- --hot-reload --config router.yaml --supergraph supergraph-schema.graphql
   ```

> If you are using managed federation you can set APOLLO_KEY and APOLLO_GRAPH_REF environment variables instead of specifying the supergraph as a file.

# Create a plugin

1. From within your project directory scaffold a new plugin
   ```bash
   cargo router plugin create hello_world
   ```
2. Select the type of plugin you want to scaffold:
   ```bash
   Select a plugin template:
   > "basic"
   "auth"
   "tracing"
   ```

   The different templates are:
   * basic - a barebones plugin.
   * auth - a basic authentication plugin that could make an external call.
   * tracing - a plugin that adds a custom span and a log message.

   Choose `basic`.

4. Add the plugin to the `router.yaml`
   ```yaml
   plugins:
     starstuff.hello_world:
       message: "Starting my plugin"
   ```

5. Run the Apollo Router and see your plugin start up
   ```bash
   cargo run -- --hot-reload --config router.yaml --supergraph supergraph-schema.graphql
   ```

   In your output you should see something like:
   ```bash
   2022-05-21T09:16:33.160288Z  INFO router::plugins::hello_world: Starting my plugin
   ```

# Remove a plugin

1. From within your project run the following command. It makes a best effort to remove the plugin, but your mileage may vary.
   ```bash
   cargo router plugin remove hello_world
   ```

# Docker

You can use the provided Dockerfile to build a release container.

Make sure your router is configured to listen to `0.0.0.0` so you can query it from outside the container:

```yml
 supergraph:
   listen: 0.0.0.0:4000
```

Use your `APOLLO_KEY` and `APOLLO_GRAPH_REF` environment variables to run the router in managed federation.

   ```bash
      docker build -t my_custom_router .
      docker run -e APOLLO_KEY="your apollo key" -e APOLLO_GRAPH_REF="your apollo graph ref" -p 4000:4000 my_custom_router
   ```

Otherwise add a `COPY` step to the Dockerfile, and edit the entrypoint:

```Dockerfile
# Copy configuration for docker image
COPY router.yaml /dist/config.yaml
# Copy supergraph for docker image
COPY my_supergraph.graphql /dist/supergraph.graphql

# [...] and change the entrypoint

# Default executable is the router
ENTRYPOINT ["/dist/router", "-s", "/dist/supergraph.graphql"]
```

You can now build and run your custom router:
   ```bash
      docker build -t my_custom_router .
      docker run -p 4000:4000 my_custom_router
   ```
