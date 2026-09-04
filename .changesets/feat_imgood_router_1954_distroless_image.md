### Publish a distroless container image alongside the Debian one

Every router release publishes a distroless image, tagged with a `-distroless` suffix:

```bash
docker pull ghcr.io/apollographql/router:<version>-distroless
```

It is built from the same router binary as the default image, on a [`distroless/cc`](https://github.com/GoogleContainerTools/distroless) base instead of `debian:bookworm-slim`. It contains the router, the shared libraries it links against, CA certificates, and time zone data, with no shell, package manager, or other userland.

See [Deploying only GraphOS Router in Docker](https://www.apollographql.com/docs/graphos/routing/self-hosted/containerization/docker-router-only#image-flavors) for the comparison between Apollo's published images.

By [@SharkBaitDLS](https://github.com/SharkBaitDLS) in https://github.com/apollographql/router/pull/10069
