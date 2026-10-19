### Rebase Docker images from Debian Bookworm to Trixie ([Issue #ROUTER-1953](https://apollographql.atlassian.net/browse/ROUTER-1953))

The published `Dockerfile.router` image and the `dockerfiles/diy/dockerfiles/Dockerfile.repo` example now build from `debian:trixie-slim` and `rust:*-slim-trixie`.

The DIY Dockerfile now also documents that it isn't intended for producing binaries that run on older-glibc hosts.

By [@SharkBaitDLS](https://github.com/SharkBaitDLS) in https://github.com/apollographql/router/pull/10032
