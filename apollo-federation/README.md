<img src="https://raw.githubusercontent.com/apollographql/space-kit/main/src/illustrations/svgs/rocket1.svg" width="100%" height="144">

[![Crates.io](https://img.shields.io/crates/v/apollo-federation.svg?style=flat-square)](https://crates.io/crates/apollo-federation)
[![docs](https://img.shields.io/static/v1?label=docs&message=apollo-federation&color=blue&style=flat-square)](https://docs.rs/apollo-federation/)
[![Join the community forum](https://img.shields.io/badge/join%20the%20community-forum-blueviolet)](https://community.apollographql.com)
[![Join our Discord server](https://img.shields.io/discord/1022972389463687228.svg?color=7389D8&labelColor=6A7EC2&logo=discord&logoColor=ffffff&style=flat-square)](https://discord.gg/graphos)

Apollo Federation
-----------------
Apollo Federation is an architecture for declaratively composing APIs into a unified graph. Each team can own their slice of the graph independently, empowering them to deliver autonomously and incrementally.

Federation 2 is an evolution of the original Apollo Federation with an improved shared ownership model, enhanced type merging, and cleaner syntax for a smoother developer experience. It’s backwards compatible, requiring no major changes to your subgraphs.

Checkout the [Federation 2 docs](https://www.apollographql.com/docs/federation) and [demo repo](https://github.com/apollographql/supergraph-demo-fed2) to take it for a spin and [let us know what you think](https://community.apollographql.com/t/announcing-apollo-federation-2/1821)!

## Usage

This crate is internal to [Apollo Router](https://www.apollographql.com/docs/router/)
and not intended to be used directly.

## Crate versioning

The  `apollo-federation` crate does **not** adhere to [Semantic Versioning](https://semver.org/).
Any version may have breaking API changes, as this API is expected to only be used by `apollo-router`.
Instead, the version number matches exactly that of the `apollo-router` crate version using it.

This version number is **not** that of the Apollo Federation specification being implemented.
See [Router documentation](https://www.apollographql.com/docs/router/federation-version-support/)
for which Federation versions are supported by which Router versions.

## Contributing

See [contributing to the `apollo-router` repository](https://github.com/apollographql/router/blob/dev/CONTRIBUTING.md)

## Security

For more info on how to contact the team for security issues, see our [Security Policy](https://github.com/apollographql/federation-next/security/policy).

## License

Source code in this repository is covered by the Elastic License 2.0. The default throughout the repository is a license under the Elastic License 2.0, unless a file header or a license file in a subdirectory specifies another license. [See the LICENSE](./LICENSE) for the full license text.
