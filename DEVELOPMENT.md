# Development
Rust implementation of federation router.

## Libs
* Configuration - Config model and loading.
* Query planner - Query plan model and a caching wrapper for calling out to the nodejs query planner.
* Execution - Converts a query plan to a stream.
* Server - Handles requests, 
  obtains a query plan from the query planner, 
  obtains an execution pipeline, 
  returns the results
  
## Binaries
* Main - Starts a server. 

## Development
We recommend using [asdf](https://github.com/asdf-vm/asdf) to make sure your nodejs and rust versions are correct.
The versions currently used to compile are specified in [.tool-versions](.tool-versions).
To set up your toolchain run:
```shell
asdf add-plugin rust
asdf add-plugin nodejs
asdf install
asdf reshim
```

The `harmonizer` dependency requires building a nodejs project. This should happen automatically, but may take some time.

Set up your git hooks:
```shell
git config --local core.hooksPath .githooks/
```

### Getting started
Use `cargo build --all-targets` to build the project.`

Some tests run against the existing nodejs implementation of the router. This requires that the `federation-demo`
project is running.

```shell
git submodule sync --recursive; git submodule update --recursive --init
cd submodules/federation-demo; npm install; 
npm run start-services &;
# Wait for the services to start 
npm run start-gateway &;
```

### Strict linting
While developing locally doc warnings and other lint checks are disabled. 
This limits the noise generated while exploration is taking place.

When you are ready to create a PR, run a build with strict checking enabled.
Use `scripts/ci-build.sh` to perform such a build.

## Project maintainers
Apollo Graph, Inc. <opensource@apollographql.com>




