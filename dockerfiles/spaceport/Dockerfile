# Start by building the application.
FROM rust:1.58.1 as builder
LABEL maintainer="gary@apollographql.com"
WORKDIR /usr/src/spaceport
RUN rustup component add rustfmt
COPY . .
RUN cargo install --path apollo-spaceport

# Now copy it into our base image.
FROM gcr.io/distroless/cc-debian11
COPY --from=builder /usr/local/cargo/bin/spaceport /usr/local/bin/spaceport
ENTRYPOINT ["/usr/local/bin/spaceport"]
