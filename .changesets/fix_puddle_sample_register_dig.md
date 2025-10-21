### Fixed authorization plugin handling of directive renames

The router authorization plugin did not properly handle authorization requirements when subgraphs renamed their authentication directives through imports. When such renames occurred, the plugin’s `@link`-processing code ignored the imported directives entirely, causing authentication constraints defined by the renamed directives to be ignored.

The plugin code was updated to call the appropriate functionality in the `apollo-federation` crate, which correctly handles both because spec and imports directive renames.

By [@sachindshinde](https://github.com/sachindshinde) in https://github.com/apollographql/router/pull/PULL_NUMBER
