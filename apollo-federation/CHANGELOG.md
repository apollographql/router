# Changelog

All notable changes to `apollo-federation` will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- # [x.x.x] (unreleased) - 2023-mm-dd

> Important: X breaking changes below, indicated by **BREAKING**

## BREAKING

## Features

## Fixes

## Maintenance
## Documentation-->

# [0.0.11](https://crates.io/crates/apollo-federation/0.0.11) - 2024-04-12

## Fixes
- Forbid aliases in `@requires(fields:)` / `@key(fields:)` argument, by [duckki] in [pull/251]

## Features
- Expose subgraphs schemas to crate consumers, by [SimonSapin] in [pull/257]

## Maintenance
- Update `apollo-compiler`, by [goto-bus-stop]

[duckki]: https://github.com/duckki
[goto-bus-stop]: https://github.com/goto-bus-stop
[SimonSapin]: https://github.com/SimonSapin
[pull/251]: https://github.com/apollographql/federation-next/pull/251
[pull/257]: https://github.com/apollographql/federation-next/pull/257

# [0.0.10](https://crates.io/crates/apollo-federation/0.0.10) - 2024-04-09

## Features
- Query plan changes for initial router integration, by [SimonSapin] in [pull/240]
- Mark join/v0.4 spec as supported for non-query planning purpose, by [SimonSapin] in [pull/233], [pull/237]
- Continued work on core query planning implementation, by [duckki], [SimonSapin], [TylerBloom]

## Maintenance
- Update `apollo-compiler`, by [goto-bus-stop] in [pull/253]

[duckki]: https://github.com/duckki
[goto-bus-stop]: https://github.com/goto-bus-stop
[SimonSapin]: https://github.com/SimonSapin
[TylerBloom]: https://github.com/TylerBloom
[pull/233]: https://github.com/apollographql/federation-next/pull/233
[pull/237]: https://github.com/apollographql/federation-next/pull/237
[pull/240]: https://github.com/apollographql/federation-next/pull/240
[pull/253]: https://github.com/apollographql/federation-next/pull/253

# [0.0.9](https://crates.io/crates/apollo-federation/0.0.9) - 2024-03-20

## Features
- Continued work on core query planning implementation, by [goto-bus-stop] in [pull/229]

## Maintenance
- Update `apollo-compiler`, by [goto-bus-stop] in [pull/230]

[goto-bus-stop]: https://github.com/goto-bus-stop
[pull/229]: https://github.com/apollographql/federation-next/pull/229
[pull/230]: https://github.com/apollographql/federation-next/pull/230

# [0.0.8](https://crates.io/crates/apollo-federation/0.0.8) - 2024-03-06

## Features
- Support legacy `@core` link syntax, by [goto-bus-stop] in [pull/224]  
  This is not meant to be a long term feature, `@core()` is not intended
  to be supported in most of the codebase.
- Continued work on core query planning implementation, by [SimonSapin], [goto-bus-stop] in [pull/172], [pull/175]

## Maintenance
- `@link(url: String!)` argument is non-null, by [SimonSapin] in [pull/220]
- Enable operation normalization tests using `@defer`, by [goto-bus-stop] in [pull/224]

[SimonSapin]: https://github.com/SimonSapin
[goto-bus-stop]: https://github.com/goto-bus-stop
[pull/172]: https://github.com/apollographql/federation-next/pull/172
[pull/175]: https://github.com/apollographql/federation-next/pull/175
[pull/220]: https://github.com/apollographql/federation-next/pull/220
[pull/223]: https://github.com/apollographql/federation-next/pull/223
[pull/224]: https://github.com/apollographql/federation-next/pull/224

# [0.0.7](https://crates.io/crates/apollo-federation/0.0.7) - 2024-02-22

## Features
- Continued work on core query planning implementation, by [SimonSapin] in [pull/121]

## Fixes
- Fix `@defer` directive definition in API schema generation, by [goto-bus-stop] in [pull/221]

[SimonSapin]: https://github.com/SimonSapin
[goto-bus-stop]: https://github.com/goto-bus-stop
[pull/121]: https://github.com/apollographql/federation-next/pull/121
[pull/221]: https://github.com/apollographql/federation-next/pull/221

# [0.0.3](https://crates.io/crates/apollo-federation/0.0.3) - 2023-11-08

## Features
- Extracting subgraph information from a supergraph for the purposes of query planning by [sachindshinde] in [pull/56]

[sachindshinde]: https://github.com/sachindshinde
[pull/56]: https://github.com/apollographql/federation-next/pull/56
