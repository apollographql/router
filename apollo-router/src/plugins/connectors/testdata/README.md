# Connectors runtime tests

Each schema in this directory is used to test the runtime behavior of connectors in the sibling `test` directory.

The runtime test require an already composed "supergraph SDL", which is the ouput of `rover supergraph compose`. Each
schema is defined using a supergraph config `.yaml` file in this directory.

## Regenerating

The `regenerate.sh` script will convert each of these `.yaml` files into a composed `.graphql` file which can be
executed.

### Options:

- Pass a specific `.yaml` file as an argument to regenerate only that file.
- Set the `FEDERATION_VERSION` environment variable to specify the federation version to use.

> [!TIP]
> If you need to compose with an unreleased version of composition, you can add any `supergraph` binary to
> `~/.rover/bin` and use the suffix of that binary as a version. For example, if you have `supergraph-v2.10.0-blah` in
> that
> bin folder, you can set `FEDERATION_VERSION="2.10.0-blah"` to use that version.
