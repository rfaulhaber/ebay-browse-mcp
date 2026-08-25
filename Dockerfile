# syntax=docker/dockerfile:1

# Fully-qualified image refs so podman needs no unqualified-search-registries
# config. Builder and runtime are pinned to the same Debian release: the binary
# links against the builder's glibc, so the runtime image must not be older.
FROM docker.io/library/rust:1-slim-bookworm AS build

# aws-lc-sys (rustls's crypto provider) builds AWS-LC from source and needs
# cmake; the slim image already carries gcc for linking.
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src

# target/ lives in a cache mount, so the binary must be copied out within the
# same RUN — the mount is not part of the layer.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked \
    && cp target/release/ebay-browse-mcp /usr/local/bin/ebay-browse-mcp

FROM docker.io/library/debian:bookworm-slim

# reqwest is built on rustls-native-certs: without ca-certificates the client
# fails at startup with "No CA certificates were loaded from the system".
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --system --uid 10001 --shell /usr/sbin/nologin mcp
USER mcp

COPY --from=build /usr/local/bin/ebay-browse-mcp /usr/local/bin/ebay-browse-mcp

# The container defaults to the remote transport (Streamable HTTP behind
# OAuth). Pass -e EBAY_MCP_HTTP_ADDR= (empty unsets it) together with
# `podman run -i` to speak MCP over stdio instead.
ENV EBAY_MCP_HTTP_ADDR=0.0.0.0:8080
EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/ebay-browse-mcp"]
