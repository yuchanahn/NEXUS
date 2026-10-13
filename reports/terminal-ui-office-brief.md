# NEXUS pixel office — owner-confirmed visual target

The owner explicitly selected: **characters working in an office (pixtuoid / Pixel Agents)**. This supersedes the earlier list-style Claude Agent View interpretation and the unresolved Flightdeck/card/graph alternatives. The default experience must be a visual office scene, not an agent table with decorative glyphs.

## References actually reviewed

- https://www.pixtuoid.dev/ — host inspected the live office demo in the browser.
- https://github.com/pixtuoid/pixtuoid — Rust terminal-native pixel-office implementation; inspect scene renderer and Source/event adapter boundaries. Its Windows integration is marked experimental; verify NEXUS on Windows directly rather than assuming compatibility.
- https://github.com/pixel-agents-hq/pixel-agents — animated characters in a small office, browser/VS Code variants. Use as a visual reference; NEXUS implementation remains Rust.

The reference scene has desks/monitors, distinguishable coworkers, office zones and animated movement. This is the intended meaning of GUI-like, even if the renderer runs inside a terminal. Existing NEXUS data/API work should feed this scene and a secondary detail/dashboard view.

## Implementation requirements

1. Build a lightweight Rust office visualizer, either by adapting reusable MIT-licensed pixtuoid components with preserved notices or by a small native scene implementation using ratatui/crossterm. Read source/licenses before choosing; report reused components and modifications. No dependency on a real Claude session or third-party paid service. Do not modify the protected NEXUS core.
2. Show actual NEXUS agents as stable named characters with individual desks. Preserve character identity across refresh/reconnect; never create duplicate coworkers just because a model call retries. Active/historical agents remain distinct; completed/stopped history is hidden or placed in an explicit archive view.
3. Drive semantic actions from actual node/agent events: model_started -> thinking; tool_started -> the appropriate working/reading/tool activity; tool result/next step -> updated action; retry/backoff -> explicit waiting/error marker; quota-paused/unreachable node -> disconnected/paused state. Use neutral unknown status when events are unavailable. A decorative idle walk must not imply useful work or actual collaboration.
4. Group nodes spatially (rooms/floors/zones): local-main, local-peer and the two active cloud nodes. Uploaded quota-paused cloud nodes must not appear as working agents. Node grouping can switch without losing selected character or activity history.
5. Real DM/board events may trigger a speech bubble or communication animation, with source/target identity preserved. Do not invent conversations or meetings. Selecting/clicking a character opens goal, model/route, actual latest event, errors and task evidence; body access remains explicit. Never render secrets.
6. Use real pixel-like shapes/colors/sprites and coherent placement of furniture, not just full-width text rows. Terminal rendering may use truecolor half-block pixels; choose a color/Unicode fallback for unsupported terminals. Windows Terminal and Linux must receive immediate keyboard/mouse input, responsive automatic refresh, viewport resize handling and clean terminal restoration. Office graphics are the default; Tab may open a factual table/graph detail view.
7. Keep visual animation local and cheap; no extra LLM calls for character movement. Fetch node data independently from rendering/input with bounded connections, native HTTPS for private cloud APIs and protected credentials. UI closure must not stop the swarm. Credential provisioning/live API integration is host-reviewed; agents never receive management secrets in their shell sandbox.
8. Preserve the useful alpha monitor as fallback until the office interface is verified. Implement this as a separately versioned UI project/release with exact launcher commands, rather than an unreviewed install that changes existing Claude/Codex hooks.

## Completion criteria

Deliver OFFICE_RESULT.md and an up-to-date README. Include executable Rust, honest component/license attribution, demo scenes visibly marked DEMO, and actual rendered office evidence. A useful first milestone: four named workers at desks, thinking/working/retrying/disconnected states, character selection with detail overlay, and responsive refresh/resize. Then connect actual local nodes and cloud nodes through the verified API/federation adapters.

Host review must visually inspect screenshots or a short terminal recording of the actual office scene, test immediate selection and resize, and compare real agent events with displayed states. Tests or synthetic screenshots alone do not establish live monitoring. Do not declare completion while the default view is still a text list. Continue federation-v1 and other useful swarm work in parallel without conflicting edits.

No Astra, no paid compute, no go-emo changes.
