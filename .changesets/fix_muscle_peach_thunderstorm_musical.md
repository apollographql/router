### Resolve Docker `unrecognized subcommand` error ([Issue #2966](https://github.com/apollographql/router/issues/2966))

We've repaired the Docker build of the v1.15.0 release which broke due to the introduction of syntax in the Dockerfile which can only be used by the the `docker buildx` tooling [which leverages Moby BuildKit](https://www.docker.com/blog/introduction-to-heredocs-in-dockerfiles/).

Furthermore, the change didn't apply to the "`diy`" (do it yourself), and we'd like to prevent the two Dockerfiles from deviating more than necessary.

Overall, this reverts [apollographql/router#2925](https://github.com/apollographql/router/pull/2925).

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/2968