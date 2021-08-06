# Development

Rust implementation of federation router.

## Crates

 *   `configuration` - Config model and loading.
 *   `query planner` - Query plan model and a caching wrapper for calling out to the nodejs query planner.
 *   `execution` - Converts a query plan to a stream.
 *   `server` - Handles requests,
     obtains a query plan from the query planner,
     obtains an execution pipeline,
     returns the results

## Binaries

 *   `router` - Starts a server.

## Development

We recommend using [asdf](https://github.com/asdf-vm/asdf) to make sure your
nodejs and rust versions are correct.  The versions currently used to compile
are specified in [.tool-versions](.tool-versions). To set up your toolchain
run:

```shell
asdf add-plugin rust
asdf add-plugin nodejs
asdf install
asdf reshim
```

The `harmonizer` dependency requires building a nodejs project. This should
happen automatically, but may take some time.

Set up your git hooks:

```shell
git config --local core.hooksPath .githooks/
```

### Getting started

Use `cargo build --all-targets` to build the project.

Some tests run against the existing Node.js implementation of the router. This
requires that the `federation-demo` project is running:

 *  If you have Docker and Docker Compose installed:

    ```
    docker-compose up -d
    ```

    **Note:** `-d` is for running into background. You can remove `-d` if you
    have issues and you want to see the logs or if you want to run the service
    in foreground.

 *  Otherwise:

    You will need Node.js and npm to be installed. It is known to be working on
    Node.js 16 and npm 7.18.

    ```shell
    git submodule sync --recursive
    git submodule update --recursive --init
    cd dockerfiles/federation-demo/federation-demo
    npm install;
    npm run start
    ```

### Strict linting

While developing locally doc warnings and other lint checks are disabled.
This limits the noise generated while exploration is taking place.

When you are ready to create a PR, run a build with strict checking enabled.
Use `scripts/ci-build.sh` to perform such a build.

## Project maintainers

Apollo Graph, Inc. <opensource@apollographql.com>
