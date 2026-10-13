# Continuous expansion mission

Updated 2026-10-13, Asia/Seoul. Current deployment status: **Docker recovered, continuous missions running**. Sixteen active goal slots are allocated; upstream failures mean these are not sixteen simultaneously productive agents. Live evidence is recorded below.

## Execution policy

An owner-created goal may set `continuous: true`; children inherit it, and the field persists through database restart. In this mode `nexus_wait` returns current board/DM headers and directs the agent to establish and execute another nonduplicated research hypothesis immediately. It does not set a future wake time. `nexus_finish` records a reported, unverified milestone and sends a parent DM; it does not end the mission. Finite goals retain their existing wait and verified completion behavior. Model failures retain exponential backoff rather than creating a retry storm.

Ready steps dispatch immediately after previous step completion. The scheduler's 500 ms timer is a fallback for newly arrived database work and retries, not an hourly agent execution schedule. Stateful providers keep their stable request prefix: fresh board/DM headers travel in successful object-valued tool results. Bodies still require explicit reads. Provider capacity is retained during active research; resource exhaustion may still delay execution.

Each node admits at most eight active goals; the two configured nodes allow four concurrent model steps in total. Coordinators can retire only their own direct children and descendants, preserving persistent files/checkpoints, to free capacity for better workers. An agent count is not evidence of useful performance. New nodes/compute must be provisioned and validated before increasing capacity.

## Assigned mission and roles

The shared goal is [continuous-expansion-goal.txt](continuous-expansion-goal.txt): discover currently callable zero-cost model suppliers; verify native/emulated tool results, coding tasks, recovery, context and collaboration; allocate useful models; grow the cooperating swarm; add external durable state and portable cloud execution. Astra and unexplained default aliases are forbidden. Paid subscriptions and hardware activation are not authorized. New executables and tests must be Rust; agents cannot change core code, core credentials or access the Docker daemon.

Main coordinator `67f9aef0-18a3-487d-9261-70f3c87e4c4f` uses the web bridge. Its seven researchers cover discovery, OpenCode, Antigravity, NVIDIA, anonymous suppliers, evaluation harness and portability. Peer coordinator `bbc0e3cf-01eb-437f-af8c-d46d123059ec` now uses the second web-model slot after NVIDIA coordinator rate limits; its researchers cover remote execution, external state, HF deployment, independent model quality, cross-node cooperation, reliability/memory and catalogue identity. Researchers use NVIDIA or OpenCode routes. Their initial goals and IDs are in ignored owner-only `projects/nexus-agent/private/expansion-*.json`.

The previous collaboration goals are stopped with original artifacts/history preserved. Consistent SQLite snapshots were taken before switching missions. Source changes were built in a separate target directory to avoid overwriting live binaries.

## Evidence and limitations

- Runtime tests: 27 passed, including continuous wait/finish, stateful DM notices, inheritance/restart, bounded spawning, protected route credentials, forbidden Astra, unrelated-agent retirement rejection, compactor transport-error recovery and oversized tool-batch result delivery. Work ownership: one test passed. Library Clippy passed. These scripted tests validate control flow; live evidence is separate below.
- Actual current preliminary supplier calls: `opencode/big-pickle` returned READY; NVIDIA Nemotron Super returned an exact random nonce and a structured `nexus_checkpoint` call. Neither is a complete agent utility assessment. Earlier probes with unsupported generation options returned 400; the successful OpenCode retry omitted those options.
- New independent Rust project `projects/nexus-cloud-state`, commit `6a83d4a`, contains a Docker event service and Rust client using a private HF Dataset as external storage. Input-validation test and build passed. The HTTP backend is not deployed or end-to-end verified.
- Existing HF CLI authentication successfully created private Dataset `yuchanahn2/nexus-swarm-state`, uploaded `owner/mission.json`, downloaded it and matched the record ID. This proves external state storage via CLI, not live cloud agent execution.
- HF rejected creation of a new private CPU-basic Docker Space with HTTP 402 requiring PRO. No paid subscription or compute was activated and no unrelated existing Space was overwritten.
- Automatic approval review rejected launching a credential-bearing state server on an external listening interface, without providing a detailed reason. That deployment was not retried through a different mechanism; the safer CLI storage verification was completed instead.
- Docker engine APIs began returning 500 while owner HTTP health still reported ready. The stalled NEXUS image build was canceled. Docker-specific restart and WSL shutdown stalled. The owner approved a whole-WSL restart, but the first elevated Windows recovery invocation returned "user canceled"; a second approved invocation successfully restarted WslService at 2026-10-13T10:51:22Z. No image/volume deletion or factory reset was performed.

## Resume and measurement

After a healthy Docker engine, `projects/nexus-agent/scripts/Resume-Expansion.ps1` starts the existing mission without recreating goals, connects coordinators by owner DM, checks the active continuous inventory and starts the existing Rust observer. Retired workers remain retired; autonomous replacements do not invalidate the inventory check. Its PowerShell code is a Windows launch wrapper; agent execution and policy remain Rust.

Earlier partial soak intervals were archived in private/soak-history. Core processes were deliberately stopped for upgrades, so those intervals cannot establish uninterrupted 24-hour operation. The latest Rust observer began at **2026-10-13T11:44:19Z (20:44:19 KST)** after the compaction regression fix and coordinator model reallocation. It is collecting; 24-hour completion is not claimed. Process/readiness sampling, actual model steps and meaningful artifacts remain distinct checks. The app heartbeat's existing PAUSED setting was preserved; the local Rust observer and node watchdogs are running independently.

## Live continuous-work evidence

- Docker engine 28.1.1 recovered, with about 31 GiB reported memory. Owner nodes started, each with two model-step slots; all sixteen initial goals inherited continuous mode.
- The web coordinator produced `/workspace/work/nexus-expansion/coordinator-contract`, executed Rust checks and posted reports to `nexus/expansion`. Its reported 25 local wire/continuation checks are not claimed as live model acceptance.
- Real swarm load exposed NVIDIA rate limits and repeated OpenCode follow-up ProviderInternal errors. These failures are recorded, not hidden as successful agent capacity. Backoff remains active.
- The owner independently verified `unclose/turboderp/Qwen3.8-27B-exl3`: actual structured `read_nonce` call, an externally generated nonce supplied only in the subsequent tool result, and an exact matching final answer.
- The coordinator read this evidence DM, registered a new descriptive Qwen route, retired its failed OpenCode integrator, and spawned **Unclose Qwen Rust Agent Pilot**. Its stated expansion gate requires real Rust source and at least four passing coding tests. This is observed autonomous allocation, not a completed quality assessment.
- The real web coordinator stalled on repeated compactor transport errors. The fix now archives the original transcript and resumes from checkpoints without claiming semantic summarization. A live `compaction_recovered` event was followed by renewed tool actions and higher step counts. Oversized tool batches now return failures for excess actions instead of discarding a whole model response.
- Sanitized current inventory: [continuous-status.json](continuous-status.json). Owner probe evidence: [provider-pilot.json](provider-pilot.json). The shared cloud Dataset round-trip is real; a remote executing cloud agent and the Rust cloud HTTP service remain unverified.



