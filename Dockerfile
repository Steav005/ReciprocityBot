# https://alexbrand.dev/post/how-to-package-rust-applications-into-minimal-docker-containers/
# Dockerfile for creating a statically-linked Rust application using docker's
# multi-stage build feature. This also leverages the docker build cache to avoid
# re-downloading dependencies if they have not changed.
FROM rust:latest AS build
WORKDIR /usr/src

RUN apt-get update
RUN apt-get install -y build-essential pkg-config musl-tools libx11-dev libfontconfig libfontconfig1-dev
# Download the target for static linking.
RUN rustup target add x86_64-unknown-linux-musl

# Create a dummy project and build the app's dependencies.
# If the Cargo.toml or Cargo.lock files have not changed,
# we can use the docker build cache and skip these (typically slow) steps.
RUN USER=root cargo new reciprocity_bot
WORKDIR /usr/src/reciprocity_bot
COPY Cargo.toml ./
RUN cargo build --release

# Copy the source and build the application.
COPY src ./src
RUN cargo install --target x86_64-unknown-linux-musl --path .

# Copy the statically-linked binary into a scratch container.
FROM scratch
COPY --from=build /usr/local/cargo/bin/reciprocity_bot .
USER 1000

CMD ["./reciprocity_bot"]