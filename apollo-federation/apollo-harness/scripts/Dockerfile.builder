# Use the rust build image from docker as our base
# renovate-automation: rustc version
FROM rust:1.72.1

# Update our build image and install required packages
RUN apt-get update && \
    apt-get -y install \
    cmake
