---
title: NEXUS Cloud Research Node
sdk: docker
app_port: 7860
---

# NEXUS Cloud Research Node

Rust persistent research node for an existing Docker Space. Uses separate Linux UIDs for the protected core/provider and extensible agent tools. No nested Docker daemon or privileged container is required. This is a process sandbox, not a separate Docker container per agent; kernel resources and the worker UID are shared within each Space. Per-agent published service ports require the Docker deployment instead.

Required Space secrets: `NEXUS_NODE_ID`, `NEXUS_ADMIN_TOKEN`, `NEXUS_PEER_TOKEN`, `NEXUS_PEERS` (JSON array of node_id/url/token), `NEXUS_HF_TOKEN`, `NEXUS_STATE_DATASET`, `NEXUS_GOAL`. Optional: `NEXUS_UPSTREAM`, `NEXUS_MODEL` (Astra forbidden).

The public `/health` endpoint confirms core readiness. Owner API uses `x-nexus-token` in addition to Hugging Face gateway authentication for private Spaces. Credentials are never provided to shell tools. Bootstrap refuses to run with a shared core/worker UID.

SQLite online backup and workspace source snapshots are saved to a private HF Dataset every 120 seconds and restored at startup. A hard crash may lose changes since the last successful snapshot. Source archives exclude target, .git and tool-output artifacts; large work must use separate artifact storage. There is no claim of gap-free 24h operation: Space sleep, upstream quotas, restarts and resource limits still apply. No paid hardware is requested.

Build the checked-in release snapshot: `docker build -t nexus-space deploy/space`. Set secrets as environment variables when starting on a Docker host. The rest of NEXUS remains independently versioned; `projects/` here is an explicit deployment source snapshot, not the live development checkouts.
