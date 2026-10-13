# Build from the NEXUS workspace root: docker build -f deploy/Dockerfile --target core .
ARG RUST_VERSION=1.94.1
FROM rust:${RUST_VERSION}-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY projects/llm-provider ./projects/llm-provider
COPY projects/nexus-agent ./projects/nexus-agent
COPY projects/nexus-agent-work/nexus-collaboration ./projects/nexus-agent-work/nexus-collaboration
RUN cargo build --locked --release --manifest-path projects/llm-provider/Cargo.toml -p provider-server \
    && cargo build --locked --release --manifest-path projects/nexus-agent/Cargo.toml \
       --bin nexus-agent --bin nexus-admin --bin nexus-tool --bin nexus-verify \
       --bin nexus-soak \
    && cargo build --locked --release --manifest-path projects/nexus-agent-work/nexus-collaboration/Cargo.toml \
       --bin nexus-collaboration --bin nexus-collab-tool

FROM rust:${RUST_VERSION}-bookworm AS worker
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates git curl ripgrep sqlite3 procps \
    && apt-get purge -y python3-minimal \
    && apt-get autoremove -y \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 10001 --create-home --shell /bin/bash agent \
    && mkdir -p /workspace/work /workspace/extensions /workspace/labs \
         /workspace/projects/llm-provider /workspace/projects/nexus-web-harness /checks \
    && chmod -R a+rX /usr/local/cargo /usr/local/rustup
COPY --from=build /src/projects/nexus-agent/target/release/nexus-tool /usr/local/bin/nexus-tool
COPY --from=build /src/projects/nexus-agent/target/release/nexus-verify /usr/local/bin/nexus-verify
COPY --from=build /src/projects/nexus-agent-work/nexus-collaboration/target/release/nexus-collab-tool /usr/local/bin/nexus-collab-tool
ENV PATH=/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin \
    HOME=/tmp/nexus-home \
    CARGO_HOME=/tmp/nexus-cargo \
    RUSTUP_HOME=/usr/local/rustup
USER 10001:10001
WORKDIR /workspace/work
ENTRYPOINT ["sleep", "infinity"]

FROM debian:bookworm-slim AS core
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates docker.io \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 10001 --create-home --shell /usr/sbin/nologin nexus \
    && mkdir -p /data /config /extensions \
    && chown -R 10001:10001 /data /config /extensions
COPY --from=build /src/projects/nexus-agent/target/release/nexus-agent /usr/local/bin/nexus-agent
COPY --from=build /src/projects/nexus-agent/target/release/nexus-admin /usr/local/bin/nexus-admin
COPY --from=build /src/projects/nexus-agent/target/release/nexus-soak /usr/local/bin/nexus-soak
COPY --from=build /src/projects/nexus-agent/target/release/nexus-verify /usr/local/bin/nexus-verify
COPY --from=build /src/projects/nexus-agent-work/nexus-collaboration/target/release/nexus-collaboration /usr/local/bin/nexus-collaboration
COPY --from=build /src/projects/nexus-agent-work/nexus-collaboration/target/release/nexus-collab-tool /usr/local/bin/nexus-collab-tool
USER 10001:10001
WORKDIR /data
ENTRYPOINT ["nexus-agent"]
