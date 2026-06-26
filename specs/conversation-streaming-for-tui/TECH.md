# TUI conversation streaming — TECH
## Context
This change adds a one-shot TUI path that sends a prompt through Warp's production AI controller and streams plain-text output to stdout. The reusable coordination model is designed to support a future interactive TUI without introducing an alternate request or response-stream implementation.
`BlocklistAIHistoryModel` stores conversations and active/progress state per terminal surface. Active conversation means the current or most recent stream/progress target; it remains distinct from the conversation selected for the next prompt (`app/src/ai/blocklist/history_model.rs:206`, `app/src/ai/blocklist/conversation_surface_model.rs`).
Every terminal surface owns a `ConversationSurfaceModel`, which is the single shared-model boundary for selected/next-prompt conversation behavior. Its GUI backend delegates Agent View presentation lifecycle to `AgentViewController`; its TUI backend owns selection without constructing Agent View UI state.
## Design
### Conversation surface boundary
GUI and TUI composition roots explicitly construct the appropriate `ConversationSurfaceModel` backend (`app/src/terminal/view.rs`, `app/src/tui/prompt_stream.rs`). The model centralizes:
- selected/next-prompt conversation lookup
- new and existing conversation targeting
- pending-query state and autoexecute override
- selection reconciliation for clear, split, remove, delete, and transfer history events
- surface-neutral Agent View lifecycle events needed by shared models

`BlocklistAIContextModel`, `BlocklistAIInputModel`, and `BlocklistAIController` each require a `ConversationSurfaceModel`. They never accept an optional `AgentViewController` and never branch on whether Agent View exists. Context owns pending attachments and request context, input owns input-mode behavior, and the controller owns request/action/stream behavior. Only `ConversationSurfaceModel` knows whether the surface is GUI or TUI (`app/src/ai/blocklist/conversation_surface_model.rs`).
### Terminal-surface-scoped history
WarpUI `EntityId` is the routing key for a terminal surface. History fields, methods, and events use `terminal_surface_id` terminology consistently:
- live, cleared, and active conversation IDs are keyed by terminal surface
- clear, restore, selection-transfer, and streaming events carry terminal surface IDs
- GUI-local APIs retain terminal-view terminology when they specifically address `TerminalView`
Moving a conversation between surfaces emits `ConversationTransferredBetweenTerminalSurfaces`, allowing the previous surface to discard rendered blocks while the destination becomes canonical (`app/src/ai/blocklist/history_model.rs:1047`, `app/src/ai/blocklist/history_model.rs:2855`).
### TUI conversation coordination
`TuiConversationModel` is the reusable TUI presentation coordinator (`app/src/tui/conversation_model.rs:31`). It contains no transcript widgets and coordinates:
- the TUI backend of the surface's selected conversation model
- creation and selection of a new conversation
- restore and selection of an existing conversation
- prompt submission through `BlocklistAIController`
- terminal-surface-filtered history events for conversation start, stream updates, status changes, selection changes, and errors
`send_prompt(...)` targets the current selection or creates a conversation when none is selected. `restore_conversation_and_send_prompt(...)` restores and selects a supplied conversation ID before delegating to `send_prompt(...)` (`app/src/tui/conversation_model.rs:116`).
### One-shot prompt streaming
`PromptStreamSurface` adapts `TuiConversationModel` events to stdout and application termination (`app/src/tui/prompt_stream.rs:57`). It is named for its actual behavior rather than as a test fixture.
TUI initialization always completes authentication before dispatching either prompt streaming or the default user-ID command (`app/src/tui.rs:23`). Prompt streaming uses the normal local terminal-manager and PTY lifecycle; there is no surface-specific PTY startup switch.
`PtySpawner` can safely use the standard terminal-server subprocess from a `warp-tui` executable. `warp::run_tui()` dispatches Warp worker invocations through the same worker runner used by `warp::run()` before starting the TUI frontend (`app/src/lib.rs:631`). Only non-worker invocations reach app-owned TUI frontend argument parsing (`app/src/tui/args.rs:5`). This lets TUI launches register the same early `PtySpawner` singleton as other Warp launches without recursively starting more TUI frontends, and preserves dispatch for other current-executable workers.
The adapter prints the local conversation ID, changed plain-text snapshots, and final status. Tool actions fail clearly because this phase does not provide approval or action UI.
### Channel-specific binaries
The `warp_tui` package mirrors GUI channel binaries. Each channel-specific binary only configures `ChannelState` and calls `warp::run_tui()`; worker dispatch and frontend argument parsing remain in the `warp` app crate. The TUI frontend accepts:
- `--prompt <text>`
- `--conversation-id <local-ai-conversation-id>`
Bare `cargo run -p warp_tui` uses the OSS/production channel; `./script/run-tui -- --prompt ...` selects the internal local channel when its channel config is available.
## End-to-end flow
```mermaid
flowchart TD
  Init["TUI initialization"] --> Auth["Authentication ready"]
  Auth --> Manager["TerminalManager<PromptStreamSurface>"]
  Manager --> Cluster["TUI-surface AI models"]
  Cluster --> Model["TuiConversationModel"]
  Model --> Select{"selected conversation?"}
  Select -->|none| New["Create and select conversation"]
  Select -->|explicit ID| Restore["Restore and select conversation"]
  New --> Send["BlocklistAIController request"]
  Restore --> Send
  Send --> Stream["ResponseStream events"]
  Stream --> History["Terminal-surface-scoped history"]
  History --> Model
  Model --> Output["Stream plain text to stdout"]
```
## Testing and validation
Automated coverage verifies:
- terminal-surface-scoped history maps and active/progress state
- GUI and TUI conversation surface selection behavior
- creating a TUI conversation selects it for the correct terminal surface
- split and removal events reconcile TUI surface selection
- selecting a new conversation, restoring an existing conversation, and sending a follow-up retain the same local conversation ID
- mock response-stream events flow through `BlocklistAIController` into filtered history/model events
- app-owned TUI frontend parsing accepts prompt-streaming CLI arguments
- Warp worker invocations dispatch before TUI frontend argument parsing
Manual validation:
- `cargo run -p warp_tui -- --prompt "Reply with exactly: hello from tui"` emits a local ID, streamed text, and `status=Success`
- a separate process using that ID with `--conversation-id` restores the conversation and recalls the previous response
- prompts requiring tools terminate with an unsupported-action error rather than hanging
Run:
- `./script/format`
- `cargo check -p warp -p warp_tui`
- `cargo check -p warp --tests`
- `cargo check -p warp --features integration_tests --tests`
- `cargo clippy -p warp -p warp_tui --all-targets -- -D warnings`
- focused nextest filters for history, context selection, TUI model, and controller response-stream tests
## Out of scope
- Transcript/rich-content TUI widgets
- TUI workspace/root orchestration UI
- TUI tool/action execution, approval UI, shell execution, or autoexecute policy
- A shared cross-surface `AgentConversationSession`
- A raw server-stream client that bypasses `BlocklistAIController`
- A stable external stdout protocol
## Risks and mitigations
- **GUI behavior regresses.** The GUI `ConversationSurfaceModel` backend delegates selection and lifecycle behavior to the existing `AgentViewController` and re-emits lifecycle events to shared models.
- **Active/progress and selected/next-prompt semantics blur.** Keep active state in history and selected/next-prompt behavior behind `ConversationSurfaceModel`.
- **A TUI selection becomes invalid.** Reconcile selection from removal, deletion, transfer, clear, and split events.
- **One-shot presentation policy leaks into reusable models.** Keep stdout, termination, and unsupported-action behavior in `PromptStreamSurface`.
