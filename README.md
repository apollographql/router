# Apollo Federation Router

Rust implementation of Federated GraphQL router.

## Usage

Apollo Federation Router requires `configuration.yaml` and `supergraph.graphql`
to be supplied.  These are either located in the default directory (OS
dependent) or explicitly specified via flag.

The router will draw its configuration from an OS dependent directory that can
be viewed via the help command.

```
OPTIONS:
    -c, --config <configuration-path>    Configuration location relative to the project directory [env:
                                         CONFIGURATION_PATH=]  [default: configuration.yaml]
    -p, --project_dir <project-dir>      Directory where configuration files are located (OS dependent). [env:
                                         PROJECT_DIR=]  [default: /home/bryn/.config/federation]
    -s, --schema <schema-path>           Schema location relative to the project directory [env: SCHEMA_PATH=]
                                         [default: supergraph.graphql]
```

To use configuration from another directory use the `-p` option.

```
router -p examples/supergraphdemo
```

This CLI is not meant to be a long term thing, as users will likely use Rover
to start the server in future.

## Project maintainers

Apollo Graph, Inc. <opensource@apollographql.com>
