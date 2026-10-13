# Existing HF Space deployment — 2026-10-13

Owner authorized repurposing all seven existing private Docker Spaces except `yuchanahn2/go-emo`. All seven received the self-contained Rust NEXUS source, Dockerfile and separate node secrets. Previous revisions are in `hf-space-rollback.json`; repositories and original assets were not deleted. `go-emo` remains RUNNING at revision `b8b23083fbdd493fe748903ee956a84abfd90ed2`, last modified 2025-08-06.

## Capacity boundary

HF enforces an account CPU Basic concurrent quota of **3**, including go-emo. `grok-image-pool` and `grok-image-script-run` occupy the two available NEXUS slots. Other five repositories are uploaded but quota-paused: runtime reports `Quota exceeded for flavor cpu-basic (requested=1): current=3, limit=3`; an explicit restart returned HTTP 403. No subscription or paid hardware was requested. Uploading code successfully does not establish a running agent.

## Validation

- Windows Rust tests: 27 runtime regressions and 2 executor authentication/identity tests passed.
- Linux Docker release built successfully, including provider-server, nexus-agent, nexus-tool and nexus-space.
- Actual HF node owner APIs are reachable through private Space authentication plus a separate x-nexus-token. Both initial nodes performed real model steps using Qwen3.8-27B via the unclose provider adapter. First node had an initial POSIX sh/pipefail error, then successful nexus_exec operations; errors were returned through tool outcomes, not discarded. Upstream timeouts also occurred, so process readiness must not be confused with uninterrupted model productivity.
- Core/provider run as UID 1000, extensible tools as UID 10001, bootstrap as root. A real tool execution confirmed UID 10001 could neither read core config/SQLite nor write the root-owned core binary. No Docker socket or nested Docker daemon is mounted. This is Linux UID/process separation within a Space, not a separate container/kernel per agent.
- Private Dataset checkpoint upload and download succeeded. A completely fresh local container restored original goal `4d8a6726-b27c-4234-bfee-0b50358a7bf7`, its accumulated steps, and the manually written `cloud-tool-ok` file. Temporary verification containers were stopped and removed.
- Two-way authenticated peer envelope insertion/read returned matching externally supplied nonces between the two cloud nodes. This was a synthetic owner-client transport test, not proof of model-initiated DM. Agents were separately instructed to perform an actual DM nonce roundtrip; inspect `hf-cloud-mesh-probe.json` and agent events for its result.
- Local Windows swarm nodes are informed of the cloud rollout but are not yet directly federated to the private cloud mesh. Do not claim local-to-cloud agent DM is established.

## Portable release

Public repository: https://github.com/yuchanahn/NEXUS . Deployment source snapshot is under `deploy/space`; source manifests record hashes and no credential files are included. Initial commit `917b77d`, WAL/archive fix `5d147fc`.

```sh
git clone https://github.com/yuchanahn/NEXUS.git
cd NEXUS
docker build -t nexus-space deploy/space
docker run --env-file /protected/node.env -p 127.0.0.1:7860:7860 nexus-space
```

Each new node needs distinct owner/peer tokens, node identity, reachable configured peer endpoints, a model route and private storage credentials. A Git clone supplies code; it does not grant these credentials or cloud capacity. HF deployment overwrites the existing entrypoint while preserving repository history.

## Durability and execution limits

Online SQLite backups and compressed work/extensions/provider-source snapshots are written every 120 seconds and on graceful supervisor shutdown. A surviving local DB/WAL is retained instead of overwritten by an older remote snapshot. A hard crash can lose work since the last successful checkpoint. Workspace archives exclude target, Git internals and tool-output artifact files; large/raw artifacts need separate persistence. A compressed workspace above 25 MB makes snapshot persistence fail visibly rather than silently truncate data. Archives are restored under worker UID, not root.

The currently supplied HF OAuth access token expires 2026-11-12 10:03:03 UTC; durable operation beyond that needs credential renewal or a separately provisioned service token. Refresh credentials were not copied. No agent receives HF management credentials in its shell environment. Existing Space inactivity timeout is 172800 seconds, but uninterrupted operation is not guaranteed by that setting. Per-agent published service ports require the standard Docker executor. This deployment is not a completed 24-hour measurement.

Latest per-node runtime/revision/model-progress observations are in `hf-space-deployment-status.json`.
