FROM rust:1.94.1-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY projects/llm-provider projects/llm-provider
RUN cargo build --release --locked --manifest-path projects/llm-provider/Cargo.toml -p provider-server
COPY projects/nexus-agent projects/nexus-agent
RUN cargo build --release --locked --manifest-path projects/nexus-agent/Cargo.toml --bin nexus-agent --bin nexus-tool --bin nexus-space

FROM rust:1.94.1-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends git curl ripgrep ca-certificates util-linux procps tar && rm -rf /var/lib/apt/lists/* && useradd --uid 1000 --create-home nexus && useradd --uid 10001 --create-home worker && mkdir -p /data /workspace/work /workspace/projects /workspace/extensions && chmod -R a+rX /usr/local/rustup /usr/local/cargo
COPY --from=build /src/projects/nexus-agent/target/release/nexus-agent /usr/local/bin/nexus-agent
COPY --from=build /src/projects/nexus-agent/target/release/nexus-tool /usr/local/bin/nexus-tool
COPY --from=build /src/projects/nexus-agent/target/release/nexus-space /usr/local/bin/nexus-space
COPY --from=build /src/projects/llm-provider/target/release/provider-server /usr/local/bin/provider-server
COPY projects/llm-provider /workspace/projects/llm-provider
USER root
WORKDIR /workspace/work
EXPOSE 7860
ENTRYPOINT ["nexus-space"]
