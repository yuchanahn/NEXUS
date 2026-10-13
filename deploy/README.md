# Portable NEXUS Docker deployment

Build and run this stack on any Docker Engine host with Compose v2. The build context is the workspace root, so the Rust agent, provider, and collaboration sources are compiled into Linux images. Runtime containers do not use a Windows path, host Python, or a Codex installation path. Persistent databases, extension manifests, and agent work are named Docker volumes. `deploy/state` contains only generated deployment configuration and credentials.

Requirements: Docker Engine or Docker Desktop with Compose v2, Git checkout of the complete NEXUS workspace, outbound network access for Cargo during image build, and a Linux/amd64 or Linux/arm64 Docker host supported by the Rust base image. To cross-build, set `DOCKER_DEFAULT_PLATFORM` (for example `linux/arm64`) before building.

On Linux, set `DOCKER_GID` in `.env` to the numeric group ID owning `/var/run/docker.sock`; the default of `0` is suitable only when the mounted socket grants group 0 access. Core needs that socket to start isolated tools. Treat this permission as host administration.

From this directory:

```sh
docker compose --profile setup build
docker compose --profile setup run --rm initialize
docker compose --profile setup run --rm seed-main
docker compose --profile setup run --rm seed-peer
```

Edit `state/provider.json` to configure an actual upstream and model, then set its credential in `state/provider.env`. Configure the provider/model fields in `state/core-main.json` and `state/core-peer.json` to match the configured provider. The initialization command creates random node, peer, collaboration, and provider credentials and refuses to overwrite an existing state directory.

Build and start:

```sh
docker compose build
docker compose up -d
docker compose ps
```

Core owner APIs are bound to loopback by default at ports 18190 and 18191; the provider is bound to 18080. Change the corresponding `MAIN_PORT`, `PEER_PORT`, and `PROVIDER_PORT` variables to publish other host ports. Do not expose owner APIs directly to the public internet. Put authenticated TLS ingress in front of them when remote access is required.

The core talks to Docker through `/var/run/docker.sock` to create isolated worker containers. Access to that socket is effectively host administration, so protect the Docker host and restrict access to the core containers. Worker containers run as UID 10001 with a read-only root filesystem, dropped capabilities, resource limits, and only dedicated workspace volumes. Images contain the Rust compiler and `nexus-tool`, `nexus-verify`, and `nexus-collab-tool` executables. The worker image removes Python and Node.js runtimes.

The deployment package is self-contained after checkout: build with `docker compose build` on the destination server. No image or volume is copied from the development machine. Back up the named volumes before upgrading. For a clean installation, keep the generated `state` directory and named volumes together; losing either the state credentials or the database volumes breaks access to the existing swarm.

Initialization also writes `state/soak.json`. The isolated `soak-observer` container runs the Rust `nexus-soak` observer, probes both core nodes and the provider, and persists its samples in the private `nexus-soak-state` volume. It has no Docker socket, no worker-network access, and no model credentials. Docker restart policies recover crashed services; the observer records health but does not control the Docker host. Its 60-second samples do not prove gap-free uptime, model-session continuity, or meaningful work. Keep the observer volume when moving or upgrading the stack if the measurement history matters. This container is isolated on the same Docker host; it does not yet implement the separate physical monitoring server envisioned for NEXUS governance.

This initializer creates a new empty swarm. It does not import the current Windows-hosted SQLite databases, agent checkpoints, or work files. Keep the old swarm intact until a Rust migration command can back up and transform that state, then verify the imported records before resuming agents. For two NEXUS deployments on different Docker hosts, replace the generated peer URLs in the core configs and `state/soak.json` with each host's reachable authenticated HTTPS endpoint; Compose service names only resolve inside this one Docker network.

This stack does not include browser-session providers. Those require a separately deployed browser harness and authenticated browser profile; this deployment does not assume the developer's local Codex or browser session exists on another host.
