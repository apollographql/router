FROM rust:1.85-slim-buster

WORKDIR /project
# circleci doesn't mount volumes with a remote docker engine so we have to copy everything
COPY --chown=1001:1001 . /project
COPY --chown=1001:1001 ~/.cargo/registry /usr/local/cargo/registry

ARG RUST_LOG
ARG RUST_BACKTRACE
ARG REDIS_VERSION
ARG REDIS_USERNAME
ARG REDIS_PASSWORD
ARG REDIS_SENTINEL_PASSWORD
ARG FRED_REDIS_CLUSTER_HOST
ARG FRED_REDIS_CLUSTER_PORT
ARG FRED_REDIS_CLUSTER_TLS_HOST
ARG FRED_REDIS_CLUSTER_TLS_PORT
ARG FRED_REDIS_CENTRALIZED_HOST
ARG FRED_REDIS_CENTRALIZED_PORT
ARG FRED_REDIS_SENTINEL_HOST
ARG FRED_REDIS_SENTINEL_PORT
ARG CIRCLECI_TESTS

RUN USER=root apt-get update && apt-get install -y build-essential libssl-dev dnsutils cmake
RUN echo "REDIS_VERSION=$REDIS_VERSION"

# For debugging
RUN cargo --version && rustc --version