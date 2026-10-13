# terminal-ui-v2 visual acceptance brief

Owner correction, 2026-10-13: the existing ANSI text dump does not satisfy the requested Claude Code-like visual agent interface. The following requirements supersede the alpha's presentation and completion criteria, while retaining already useful read-only API work. Do not mark a parser/test-only milestone as UI completion.

## Verified visual references

Host reviewer visually inspected the official Claude Code terminal UX image and the official Agent View screenshot (including opening its enlarged view in the browser):

- https://code.claude.com/docs/en/agent-view
- https://code.claude.com/docs/en/fullscreen
- https://code.claude.com/docs/en/sub-agents
- https://www.anthropic.com/news/enabling-claude-code-to-work-more-autonomously

Agent View's official screenshot presents a compact summary at the top, state groups separated by whitespace, rows with state icon/name/current activity/time, a highlighted selected row, and a fixed bottom task input plus keyboard hints. Official documentation describes color/animation for status, a Space peek panel, Enter attachment to a transcript and navigation back. The terminal welcome screenshot adds subdued accent color, bordered information areas and a recognisable small terminal illustration. These are references, not a request to copy Anthropic branding, text or artwork. Do not claim NEXUS requires Claude or invokes Claude models to draw this UI.

## Required NEXUS terminal experience

Use Rust ratatui + crossterm (or an equivalently capable cross-platform terminal renderer). Online Cargo dependency retrieval is authorized; the minimal serde_json-only/offline alpha was a bootstrap choice, not an owner constraint. Implement actual layout and interaction now, retaining core protection and the other ongoing federation work.

1. Stable alternate-screen layout with intentional spacing, aligned columns, restrained color palette, selection highlight and fixed footer. Redraw only as needed, preserve focus/scroll during refresh, and restore terminal state on exit/error. No wall of repeated full-screen println output.
2. Overview: compact NEXUS header showing reachable nodes, active workers, retries and unavailable/quota-paused nodes separately. Group meaningful rows by attention-needed, working, retry/backoff, queued and historical results; history collapsed by default. Do not label all historical records as active agents or treat running model requests as proven productivity.
3. Agent row: colored status glyph, readable name, node, concise latest known activity, age/last successful step and route/model when actually known. Animate a small spinner only for an actual running state. Unknown metrics remain unknown; do not invent completion percentages, tokens, resource graphs or model names.
4. Up/Down immediately select rows on Windows Terminal and Linux. Space opens a compact peek panel with goal, latest event/error and progress evidence. Enter opens the selected agent's full scrollable activity/tool timeline; Esc/Left returns without stopping the agent. Tab changes view/focus; slash opens search/filter; question mark opens help. No Enter required after every navigation key.
5. Support state/node grouping and a goal/parent hierarchy view with indentation, expanding/collapsing descendants and readable paths back to the root. Selection follows agent identity rather than changing accidentally when refresh reorders rows.
6. Refresh data automatically in the background, independently of user keyboard input, with bounded networking, stale indicators and explicit disconnected state. Keyboard input and terminal resizing remain responsive while any endpoint is slow or unreachable. Status animation must not trigger LLM requests.
7. Transcript view distinguishes model response, tool call, result, retry and checkpoint visually; long tool output collapses until opened. Show factual recent events, not one static copied goal as alleged current activity. Surface board/DM headers where authenticated APIs actually support them; unsupported APIs get an explicit disabled indication without constant error spam.
8. Bottom input in v2 supports local search/commands. A real message composer may be added using an explicitly configured owner message API; it must show the selected recipient, only transmit on intentional submission and show acceptance/error. Do not show a fake working command/message field. No destructive stop/delete controls are required for this visual milestone.
9. Native HTTPS for cloud nodes, with distinct gateway and endpoint-specific credentials from protected configuration/environment; actual local endpoints are /v1/agents and /v1/peer/board, and GET /v1/messages does not exist. Keep credentials out of render buffers, logs, fixtures and source. Agent sandbox never receives real host credentials.

## Visual review is a completion gate

- Provide demo scenarios for healthy multi-node work, retries/quota-paused nodes and node disconnection; visibly label fixture data DEMO.
- Deliver at least three actual rendered terminal frames (overview, peek/transcript, narrow/resize) at representative sizes such as 120x35 and 80x24. ratatui TestBackend snapshots are useful automated layout checks, but are not a substitute for reviewer inspection of actual Windows Terminal/Linux terminal rendering. Screenshots/cast recordings may be produced in a supported environment; otherwise request host visual review explicitly in RESULT.md.
- Verify immediate navigation, automatic refresh while typing, selection preservation, resize, long names/output and clean terminal restoration. Reuse API tests where useful; do not inflate test counts as a substitute for the visual surface.
- Final RESULT.md and README must describe the implemented interface and exact launch commands honestly. The host will verify real local/cloud connectivity using protected credentials and compare the actual rendered UI with this brief before declaring delivery.

Keep the current working alpha available as a fallback until v2 is verified. New dependencies and UI changes belong in the separate nexus-tui project, not in the protected core. Astra remains forbidden.
