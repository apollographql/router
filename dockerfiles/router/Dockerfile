# Start by building the application.
FROM rust:1.58.1 as builder
LABEL maintainer="gary@apollographql.com"
WORKDIR /usr/src/router
RUN apt-get update
RUN apt-get -y install \
    npm \
    nodejs
RUN rustup component add rustfmt
COPY . .
RUN cargo install --path apollo-router

# Now copy it into our base image.
FROM gcr.io/distroless/cc-debian11
COPY --from=builder /usr/local/cargo/bin/router /usr/local/bin/router
ENTRYPOINT ["/usr/local/bin/router"]
