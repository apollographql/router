### Improve Changelog management through conventions and tooling ([PR #2545](https://github.com/apollographql/router/pull/2545), [PR #2534](https://github.com/apollographql/router/pull/2534))

New tooling and conventions adjust our "incoming changelog in the next release" mechanism to no longer rely on a single file, but instead leverage a "file per feature" pattern in conjunction with tooling to create that file.

This stubbing takes place through the use of a new command:

    cargo xtask changeset create

For more information on the process, read the [README in the `./.changesets` directory](https://github.com/apollographql/router/blob/HEAD/.changesets/README.md) or consult the referenced Pull Requests below.

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/2545 and https://github.com/apollographql/router/pull/2534