# The Butterbot image: one statically linked program in an otherwise empty
# image. The same file builds linux/amd64 and linux/arm64; each platform is
# compiled by a compiler of that platform, so nothing is cross-compiled.
#
#   docker build --tag butterbot .
#   docker buildx build --platform linux/amd64,linux/arm64 --tag butterbot .

# Keep in step with rust-toolchain.toml.
ARG RUST_VERSION=1.97.0

FROM rust:${RUST_VERSION}-alpine AS build
# A C compiler and CMake for the cryptography library. On Alpine the program
# is linked against musl, statically.
RUN apk add --no-cache build-base cmake
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked --bin butterbot \
    && cp target/release/butterbot /butterbot

# For the container tests only (`docker build --target test .`): the program
# with the settings that point it at the ACME test server. A build without
# `--target` skips these two stages.
FROM build AS build-test
RUN cargo build --release --locked --bin butterbot --features test-support \
    && cp target/release/butterbot /butterbot
FROM scratch AS test
COPY --from=build-test /butterbot /usr/local/bin/butterbot
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/butterbot"]
CMD ["run"]

# The image.
FROM scratch
COPY --from=build /butterbot /usr/local/bin/butterbot
# An unprivileged user. The image has no user database, so it is a number.
USER 65532:65532
# Arguments given to the container replace `run`: `docker run <image> status`.
ENTRYPOINT ["/usr/local/bin/butterbot"]
CMD ["run"]
