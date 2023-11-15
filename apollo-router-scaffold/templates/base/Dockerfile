# Use the rust build image from docker as our base
# renovate-automation: rustc version
FROM rust:1.72.0 as build

# Set our working directory for the build
WORKDIR /usr/src/router

# Update our build image and install required packages
RUN apt-get update
RUN apt-get -y install \
    npm \
    protobuf-compiler

# Add rustfmt since build requires it
RUN rustup component add rustfmt

# Copy the router source to our build environment
COPY . .

# Build and install the custom binary
RUN cargo build --release

# Make directories for config and schema
RUN mkdir -p /dist/config && \
    mkdir /dist/schema && \
    mv target/release/router /dist

# Copy configuration for docker image
COPY router.yaml /dist/config.yaml

FROM debian:bullseye-slim

RUN apt-get update
RUN apt-get -y install \
    ca-certificates 

# Set labels for our image
LABEL org.opencontainers.image.authors="Apollo Graph, Inc. https://github.com/apollographql/router"
LABEL org.opencontainers.image.source="https://github.com/apollographql/router"

# Copy in the required files from our build image
COPY --from=build --chown=root:root /dist /dist

WORKDIR /dist

ENV APOLLO_ROUTER_CONFIG_PATH="/dist/config.yaml"

# Default executable is the router
ENTRYPOINT ["/dist/router"]
