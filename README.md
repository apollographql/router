# Router
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

The `harmonizer` dependency requires building a nodejs project. This should happen automatically, but may take some time.

### Getting started

Use `cargo build --all-targets` to build the project.`

### Strict linting
While developing locally doc warnings and other lint checks are disabled. 
This limits the noise generated while exploration is taking place.

Once you are ready to create a PR you will need to run a build with strict checking enabled.
Use `scripts/ci-build.sh` to perform such a build.

## Project maintainers
Apollo Graph, Inc. <opensource@apollographql.com>




