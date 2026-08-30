# Static musl build so the relay drops onto any Linux host without touching its
# toolchain or glibc. Build context is this directory only.
#
#   docker build -t mirror-x-relay-build --target artifact .
#   docker create --name extract mirror-x-relay-build
#   docker cp extract:/codex-plus-mobile-relay ./codex-plus-mobile-relay
FROM rust:1.85-alpine AS build

RUN apk add --no-cache musl-dev
WORKDIR /src

COPY deploy/Cargo.standalone.toml ./Cargo.toml
COPY src ./src
COPY pwa ./pwa

RUN cargo build --release && ls -la target/release/codex-plus-mobile-relay

FROM scratch AS artifact
COPY --from=build /src/target/release/codex-plus-mobile-relay /codex-plus-mobile-relay
