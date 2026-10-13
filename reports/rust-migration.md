# NEXUS Rust migration status

Updated: 2026-10-13 (Asia/Seoul)

## Current state

The autonomous swarm has resumed on its preserved local databases. The `nexus-24` heartbeat is active. A fresh 24-hour readiness measurement began after the Rust observer's initial probe-format bug was corrected; the measurement is collecting and is not complete.

Rust is already the implementation language for the agent core, provider contract/server/adapters, and the new collaboration service/client. The Rust collaboration crate currently passes three integration tests and Clippy. Existing collaboration Python sources and Python acceptance checks still exist and have not yet been removed or fully replaced.

## Portable Docker work completed in this pass

- Added `deploy/Dockerfile`, which builds Linux Rust binaries from the workspace source for core, collaboration, provider dependencies, and worker tools.
- Added `deploy/compose.yaml` for the provider, two core nodes, two collaboration nodes, named persistent volumes, and an isolated worker network.
- Added Rust-generated deployment setup (`nexus-admin init`) and extension seeding (`nexus-admin seed`) to the documented setup flow.
- Built `nexus-core:local`, `nexus-worker:rust`, and `nexus-provider:local` with Docker Desktop's Linux/amd64 engine.
- Ran the Rust setup CLI inside the Linux core image and rendered the Compose configuration successfully.
- Verified the worker image includes the Rust tools/compiler and has no Python or Node executable.
- Replaced the 24-hour Python observer with Rust `nexus-soak`. It authenticates to both local nodes, probes provider/owner/harness readiness without assuming JSON response bodies, appends private JSONL samples, atomically writes the public status, prevents Windows sleep, rejects duplicate observers, and attempts the existing recovery scripts. Rust unit test and Clippy pass. The first post-fix sample reports both nodes and all three services ready with zero failures.
- Added a config-driven remote observer mode, Rust-generated `soak.json`, and an isolated Compose observer service. It has no Docker socket or worker-network access. Config generation and remote health sampling have Rust tests; the destination Linux image build is still running, so the container mode is not yet confirmed.
- The Windows local observer still invokes PowerShell recovery scripts and uses local ports/configuration. The Compose observer only records readiness; Docker restart policies handle crashed containers. This container runs on the same Docker host, not the separate physical governance server envisioned for the future.
- Provider image build ran its Cargo workspace tests and Clippy successfully.
- Core tests passed: 19 runtime tests and one work-claim test. Collaboration tests passed: 3. A physical second-server deployment has not been exercised.

## Remaining migration

- Replace the remaining agent setup, status, goal launch, and verification Python scripts with Rust commands and update Windows launch wrappers. The soak observer is now Rust, but its PowerShell recovery boundary remains.
- Finish moving collaboration service/client behavior and test coverage to Rust, then retire the Python implementation and acceptance scripts.
- Convert or explicitly retire the one-off Python provider research probes under `labs`; the web harness still has 169 TypeScript/JavaScript source files (about 59,350 lines), so the browser automation boundary has not been ported to Rust.
- Add a data-preserving migration from the paused Windows-hosted swarm to Docker named volumes. The current Compose setup provisions a fresh stack; it does not yet import the paused databases, checkpoints, credentials, or worktree contents.
- Run the stack end-to-end from a clean checkout on a distinct Docker server, including worker creation, restart recovery, and cross-node collaboration.

The generated local `deploy/state` contains deployment credentials and is excluded from Git and Docker build contexts. It has not been used to start containers or create goals. The resumed local swarm is separate from that fresh Compose deployment.
