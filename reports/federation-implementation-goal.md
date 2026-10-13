# NEXUS local/cloud federation implementation mission

Owner directive, 2026-10-13: give the deployed cloud agents useful work and implement everything needed to cooperate with the existing local swarm. This is part of the existing continuous expansion mission, not a new architecture-only exercise.

## Current facts

Two cloud nodes are active: hf-grok-image-pool (root 8035efc5-6b0a-4ead-b4bb-b0fde2992afb), hf-grok-image-script-run (root 771420ce-e060-4fd4-9a8e-ff02cfbe4ada). Their observed steps advanced from 5/3 to 7/6; each also recorded four errors. Other five uploaded nodes are HF quota-paused. Account CPU Basic concurrency is 3 including protected go-emo, so only two cloud compute slots are available. A running process is not proof of productive model progress.

Private HF peer transport has a synthetic bidirectional nonce test. Local/cloud agent federation and model-initiated DM roundtrip are not established merely by that transport test. Existing remote-worker code is only Hello world and is not a usable transport.

## Required implementation

1. Build a separately versioned Rust federation gateway/mailbox adapter outside the protected core. First evaluate an outbound local connector with an authenticated durable cloud mailbox (poll/long-poll/stream), avoiding dependence on inbound access to the local PC. A tunnel is an optional deployment alternative, not an assumed prerequisite. Specify the actual remote mailbox endpoints and local peer facade; existing GET /v1/messages is absent and cannot serve as a mailbox.
2. Register distinct node and agent identities, owner-provisioned peer trust, capabilities and a common mission identifier. Owner credentials, scoped peer credentials and HF gateway credentials remain separate. Validate actual private Space gateway Authorization plus inner x-nexus-token. No credential values enter agent prompts, shell environments, source, UI output or public Git.
3. Preserve sender provenance for cross-node board entries and DM. Never relabel remote agent data as a human instruction. If an adapter wraps an envelope under its registered peer identity, retain and verify the original sender/envelope explicitly. Inject only unread headers at safe model boundaries; body retrieval is explicit.
4. Implement durable outbox/inbox, ACK, deduplication, retries/backoff, ordering where required, expiration and recovery after process/network outages. Define delivery semantics honestly (normally at-least-once with idempotent handling), not unproven exactly-once execution. Store queues in protected SQLite or equivalent durable storage; do not assume HF Dataset file reads provide an atomic distributed work claim.
5. Support disjoint task assignment, claims/leases, evidence/artifact publication and result retrieval across local and cloud nodes. Shared global mission must survive restart, while node-specific tasks remain distinguishable. Scheduling respects healthy model routes, quota-paused nodes, slot capacity and actual productivity; avoid repeated probes or purchases to bypass quota.
6. Supply a portable Docker deployment bundle, credential-free configuration examples, enrollment/bootstrap scripts and rollback procedures. New clone/build supplies source, not authorization or cloud capacity. Core changes are forbidden to agents: any strictly necessary core integration must be a minimal owner-reviewed proposal with fixtures and config requirements, not an agent edit to protected code.
7. Feed node availability, authenticated connectivity, backlog/ACK age, last successful model/tool step and useful task outcomes into terminal-ui-v1. Show unavailable/stalled/retrying separately. Do not expose management secrets to the UI process unnecessarily.

## Work split

- Local-main coordinator: integrate this into the global expansion plan; task/result catalog, assignment/claims, UI integration and no duplicate work.
- Local-peer coordinator: lead executable gateway/mailbox/connector integration and deployment evidence. Prefer finishing implementation itself if workers are rate-limited; do not endlessly delegate to failing routes.
- Cloud pool researcher: small Rust wire/provenance/nonce contract crate, serialization/validation and attack/replay fixtures.
- Cloud script-run researcher: small Rust durable queue/ACK/recovery crate with bounded retry and restart tests, coordinated with the gateway leader.

Expected results: separately versioned nexus-federation project and FEDERATION_RESULT.md with source paths, exact commands, tests, limitations and protected deployment inputs. Existing terminal UI work must continue.

## Acceptance evidence

- Real local agent -> cloud agent DM, explicit body read and nonce reply -> local agent, with accurate sender identities; distinguish from a synthetic operator test.
- Local agent assigns one disjoint Rust task; cloud agent actually runs it, publishes artifact/evidence and local agent retrieves/verifies the result.
- Disconnect/reconnect and restart recover queued messages and checkpointed tasks without silently dropping them or repeating accepted side effects.
- Fresh Docker installation joins with injected configuration, without Windows-specific paths or developer browser/Codex state.
- Both authorization failure and unsupported/quota-paused nodes display explicit unavailable state.
- No Astra, paid compute, go-emo changes or unrestricted public owner API exposure.

Operator integration of real protected credentials and host configuration is a final deployment step after agents deliver concrete code. Sandbox-only test success is not live federation validation.
