# Configuration migrations
This directory contains configuration migrations that can be applied to a router config to bring it up to date with current config format.

It uses [proteus](https://github.com/rust-playground/proteus) under the hood, which handles the complexities of merging Json.

A migration has the following format:

The filename should begin with a 4 digit numerical prefix. This allows us to apply migrations in a deterministic order.
`Filename: 0001-name.yaml`. It must start with the current major version of the router. For example for router 2.x it should start with `2001-name.yaml`. If it doesn't start with the right version then it would be considered as a real breaking change and won't be automatically migrated when the router starts.

The yaml consists of a description and a number of actions:
```yaml
description: telemetry.tracing.common.attributes.router has been renamed to 'supergraph' for consistency
actions:
  - type: move
    from: some.source
    to: some.destination
  - type: copy
    from: some.source
    to: some.destination
  - type: delete
    path: some.destination
  - type: add
    path: some.destination
    name: someName
    value: someValue
  - type: change
    path: some.destination
    from: oldValue
    to: newValue
  - type: log
    level: error
    path: some.source
    log: this field is not longer available because XXX
```

Each action is applied in order. Use the following formats for from, to and path.

## Actions

| action | description |
---------|-------------|
| move | Moves the value at `from` to `to`, removing `from`. No-op if `from` doesn't exist. |
| copy | Copies the value at `from` to `to`, leaving `from` in place. No-op if `from` doesn't exist. |
| delete | Removes the value at `path`. No-op if `path` doesn't exist. |
| add | Sets `path.name` to `value`. Only applies if `path` already exists and `path.name` does not — parent structure is never created. |
| change | Replaces the value at `path` with `to`, but only if it currently equals `from`. No-op if `path` doesn't exist or holds a different value. `from` and `to` are compared/written as whole YAML values, so either may be a scalar, a map or a list. |
| log | Migrates nothing; just logs `log` at `level` when `path` matches something. Useful when moving a feature from experimental to GA in a way that isn't backward compatible. |

## Getter (from)
| syntax | description |
---------|-------------|
| | this will grab the top-level value which could be any valid type: Object, array, ... |
| id | Gets a JSON Object's name. eg. key in HashMap |
| [0] | Gets a JSON Arrays index at the specified index. |
| profile.first_name | Combine Object names with dot notation. |
| profile.address[0].street | Combinations using dot notation and indexes is also supported. |

## Setter (to, path)
| syntax | description |
---------|-------------|
| | this will set the top-level value in the destination |
| id | By itself any text is considered to be a JSON Object's name. |
| [] | This appends the source **data** to an array, creating it if it doesn't exist and is only valid at the end of set syntax eg. profile.address[] |
| [\+] | The source Array should append all of it's values into the destination Array and is only valid at the end of set syntax eg. profile.address[] |
| [\-] | The source Array values should replace the destination Array's values at the overlapping indexes and is only valid at the end of set syntax eg. profile.address[] |
| {} | This merges the supplied Object overtop of the existing and is only valid at the end of set syntax eg. profile{} |
| profile.first_name | Combine Object names with dot notation. |
| profile.address[0].street | Combinations using dot notation and indexes is also supported. |

See [proteus](https://github.com/rust-playground/proteus) for more options.

If a migration is deemed to have changed the configuration then the description of the migration will be output to the user as a warning.

In future we will be able to use these files to support offline migrations.

# Testing
Once you have made a new migration place a config file in `testdata/migrations`. It will automatically be picked up by the `upgrade_old_configuration` test.
