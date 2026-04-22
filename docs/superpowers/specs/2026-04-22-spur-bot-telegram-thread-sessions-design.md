# SPUR Bot Telegram Thread-Oriented Sessions Design

**Date:** 2026-04-22
**Status:** Approved
**Scope:** `spur-bot`, `spur-cli`, Telegram private-chat topics, per-thread session routing
**Builds On:** [2026-04-22-spur-bot-design.md](./2026-04-22-spur-bot-design.md)

---

## Problem Statement

The current `spur-bot::telegram` implementation is intentionally single-surface:

- one private DM
- one sticky current session
- one global current binding in persisted state

That shipped the brain-first Telegram bot, but it is the wrong abstraction for
multi-session operation inside one Telegram conversation.

Telegram now supports topic/thread mode in private chats for bots. That creates
a better frontend surface for SPUR:

- the private chat can become a control-plane lobby
- each Telegram topic can become a dedicated SPUR conversation surface
- multiple SPUR sessions can coexist without hidden global session switching

The design problem is to map Telegram thread semantics onto SPUR session
semantics without breaking existing SPUR invariants around:

- ACP session identity
- event-driven binding updates
- review and permission routing
- restart-safe restore behavior

---

## Product Goal

Extend `spur-bot::telegram` so that a single operator can manage **multiple
thread-oriented SPUR sessions** inside one private Telegram chat.

The target model is:

- the main private chat is a lobby and control plane
- `/new` creates a Telegram topic
- each topic owns at most one live SPUR session binding
- chatting with the brain happens only inside topics
- reviews and permission prompts stay in the same topic as the session they
  belong to
- closed or deleted topics do not destroy SPUR session history; bindings become
  archived and detached

---

## Non-Goals

This design does not attempt to solve:

- multi-user Telegram operation
- shared group or supergroup control surfaces
- Discord or other transports
- generalized transport-neutral thread abstractions for all future platforms
- automatic semantic topic naming from prompts
- eager restore of all persisted thread bindings at startup

Those can be layered later without changing the core thread-native model.

---

## Telegram Grounding

This design is grounded in the published Telegram Bot API:

- Telegram supports topics in private chats for bots when forum-topic mode is
  enabled
- messages in topics carry `message_thread_id`
- bots can create topics with `createForumTopic`
- bots can target topic messages with `message_thread_id`
- bots can edit topic titles later with `editForumTopic`

Primary sources:

- [Telegram Bot API changelog](https://core.telegram.org/bots/api-changelog)
- [createForumTopic](https://core.telegram.org/bots/api#createforumtopic)
- [editForumTopic](https://core.telegram.org/bots/api#editforumtopic)
- [User](https://core.telegram.org/bots/api#user)
- [sendMessageDraft](https://core.telegram.org/bots/api#sendmessagedraft)

Two Bot API rules are particularly important for implementation correctness:

- topic-capable operation must be gated by bot capability checks, especially
  `getMe().has_topics_enabled`
- the General topic is special; `message_thread_id = 1` should be treated as
  the lobby / General surface on inbound updates, while outbound sends to the
  lobby must omit `message_thread_id`

One implementation constraint should be made explicit:

- the Bot API surface does not provide a good bot-level “list all topics”
  operation comparable to MTProto topic listing

So the SPUR design must treat topic discovery as:

- bot-created topics
- or inbound-topic updates already seen by the bot

It must not rely on enumerating all Telegram topics from the server.

---

## Approved Decisions

### Topic Surface

V1 thread-oriented multi-session support targets **private-chat topics** first.

This keeps the operator model unchanged:

- one operator
- one private Telegram chat
- many topic threads within that chat

The design does not attempt to solve forum supergroup topics yet.

### Session Ownership

Each Telegram topic owns **at most one live SPUR session binding**.

This is the core mapping:

- Telegram durable identity: `(chat_id, message_thread_id)`
- SPUR durable identity: `acp_session_id`

Those two durable identities should map directly.

### Topic Creation

`/new` in the lobby creates a **new Telegram topic** and binds a new SPUR
conversation surface to that topic.

The bot does **not** auto-create topics from plain text in the lobby.

### Lobby Behavior

The main private chat becomes a **lobby / control plane**.

It is responsible for:

- `/new`
- `/sessions`
- `/help`
- status and setup errors

It is **not** a brain chat surface once topics are enabled.

### Resume and Rebinding

`/resume <id>` is valid only **inside a topic**.

If that topic already has a live bound session, the old binding is:

- detached from the topic
- preserved as archived/detached in the registry

The new session then becomes the topic’s live binding.

### Topic Naming

`/new` uses **sequential default names**:

- `Session 1`
- `Session 2`
- …

Topic titles are presentation metadata, not identity. Richer context belongs in
the lobby `/sessions` output, not in the topic name itself.

### Topic Deletion / Closure

If a topic is deleted or closed, the SPUR binding is **not discarded**.

Instead:

- the thread record becomes archived/detached
- the ACP session remains visible in `/sessions`
- another topic can later bind to that session with `/resume <id>`

Telegram topic lifecycle must not become the authority for SPUR session
existence.

---

## Best-Approach Decision

Three architectures were evaluated:

1. **Thread-native runtime keys**
2. **Global current-session runtime plus thread adapters**
3. **A broader transport-neutral multi-surface runtime first**

The correct choice is **thread-native runtime keys**.

Why:

- Telegram’s real sub-surface is the topic/thread
- SPUR’s real conversation authority is the ACP session
- one topic → one live session is the cleanest identity mapping
- prompt routing, restore semantics, and render targeting all become explicit

The rejected alternatives fail for different reasons:

- global current-session runtime fights the product model and introduces hidden
  cross-thread switching
- over-generalizing into a new cross-transport abstraction now would solve a
  larger architecture problem than this feature requires

One refinement matters:

- restore should be **lazy per thread**, not eager across all persisted
  bindings

This avoids fan-out session resumes on startup and keeps restore behavior tied
to real topic activity.

---

## Architecture

### Core Model

The runtime should become explicitly thread-aware.

Introduce:

```rust
ThreadKey {
    chat_id: i64,
    message_thread_id: Option<i32>,
}
```

Interpretation:

- `message_thread_id = None` means the lobby
- `message_thread_id = Some(id)` means a topic-backed session surface

Normalization rule:

- inbound `message_thread_id = 1` must be normalized to `None`
- outbound sends to the lobby must omit `message_thread_id` rather than sending
  `1`

The lobby is a first-class routing surface, but it does not own a live brain
session binding.

### Thread Records

Persisted and in-memory runtime state should use thread records, not one global
binding.

Each topic record should contain:

- `thread_id`
- `topic_name`
- `archived`
- optional `acp_session_id`
- optional `brain`
- binding state

Binding state:

- `Unbound`
- `RestorePending`
- `Active`
- `ArchivedDetached`

### Invariants

The runtime must maintain these invariants:

- the lobby never owns a live brain binding
- each live topic owns at most one live session binding
- no ACP session may be live-bound to multiple topics simultaneously
- archived bindings remain visible and resumable
- persisted live bindings load as `RestorePending`, not `Active`
- `AgentSessionReady` remains the only commit point for activating a binding

---

## Command Model

### Lobby Commands

Valid in the lobby:

- `/new`
- `/sessions`
- `/help`

Lobby plain text is rejected with a short instruction to:

- create a new topic with `/new`
- or enter an existing topic to continue chatting

`/new` behavior:

1. create topic `Session N`
2. persist a new `Unbound` thread record
3. post a starter message inside the new topic
4. wait for the first plain-text message in that topic to start the SPUR
   session

`/sessions` behavior:

- list thread-oriented entries, not just raw ACP ids
- each entry should include:
  - topic name
  - binding state
  - ACP session id when present
  - archived/detached state when applicable

### Topic Commands

Inside a topic:

- plain text
- `/resume <id>`
- `/current`
- `/cancel`

`/new` is invalid inside a topic and should return:

- `Use /new in the lobby to create a topic.`

Plain text behavior:

- `Unbound` -> `NewSessionWithMessage`
- `RestorePending` -> queue the message, send `ResumeSession`, flush only after
  `AgentSessionReady`
- `Active` -> `Message`
- `ArchivedDetached` -> reject with a short instruction to resume or create a
  new topic

`/resume <id>` behavior:

- valid only inside a topic
- archives any current live binding for that topic
- attempts to bind the requested ACP session to that topic
- leaves the existing binding unchanged if resume fails

`/current` reports the current topic’s binding state.

`/cancel` only affects the topic’s active session.

---

## Prompt and Output Routing

Review and permission routing must remain **topic-local**.

This means:

- every prompt token record stores `ThreadKey`
- every render target stores `ThreadKey`
- callbacks are validated against both token liveness and thread binding state

Rules:

- review prompts render in the same topic as the session they belong to
- permission prompts render in the same topic as the session they belong to
- stale callbacks for archived or rebound topics are answered cleanly and do
  not mutate runtime state

Normal chat output should also remain topic-local:

- `AgentNotification` chunks accumulate per ACP session
- `TurnComplete` flushes the session buffer into a final topic message
- `BrainError` clears the session buffer and surfaces a service message in that
  same topic

---

## Restore Behavior

Restore must be **lazy per thread**.

At startup:

- load the registry
- reconstruct the lobby implicitly
- load every persisted live binding as `RestorePending`
- do not eagerly call `ResumeSession` for every topic

When the operator sends plain text into a `RestorePending` topic:

1. queue the user message
2. send `ResumeSession`
3. wait for `AgentSessionReady`
4. mark the topic `Active`
5. flush the queued message to the resumed session

This preserves the current event-driven SPUR invariant while avoiding startup
fan-out and hidden cross-thread switching.

---

## Persistence Model

The current single-binding file should become a registry-shaped file.

This requires an explicit state migration rule from the current flat
`PersistedBotState`:

- a legacy file with one `current_acp_session_id` and one `current_brain`
  should be loaded into the new schema without data loss
- if no topic registry exists yet, the legacy binding should be treated as a
  detached archived session record or as a lobby-only legacy binding that
  requires explicit rebinding before topic-local chat begins
- the implementation plan must choose one of those migration paths explicitly;
  it may not silently drop the old single-session state

Persist:

- `version`
- `operator_chat_id`
- `next_topic_seq`
- thread records keyed by `message_thread_id`
- archived state
- optional index information needed for fast ACP session lookup

Rough shape:

```rust
PersistedBotState {
    version: u32,
    operator_chat_id: Option<i64>,
    next_topic_seq: u32,
    threads: HashMap<i32, PersistedThreadRecord>,
}
```

Where each thread record contains:

- `topic_name`
- `archived`
- optional `acp_session_id`
- optional `brain`
- persisted binding state sufficient to reconstruct `Unbound`,
  `RestorePending`, or `ArchivedDetached`

The lobby does not need an explicit persisted thread record.

Atomicity requirements:

- rebinding a topic must update the topic record and ACP-session index together
- archiving a previous binding during `/resume` must not leave two live topic
  owners for the same session

---

## Transport Changes

### Router

`spur-bot::telegram::router` must propagate `message_thread_id` on:

- inbound text messages
- inbound callback queries

Callback routing must read the thread identity from
`callback_query.message.message_thread_id`.

The router should also treat `is_topic_message` as defensive context when that
field is present, but `message_thread_id` remains the authoritative routing
input.

This is what turns the runtime from global-session routing into topic-local
routing.

### Client and Render Paths

Telegram send/edit/draft helpers must accept optional `message_thread_id`.

When the target is a topic:

- `sendMessage`
- `sendChatAction`
- `sendMessageDraft`
- prompt rendering

must all target the correct thread.

The lobby uses `message_thread_id = None`.

Outbound rule:

- if the render target is the lobby / General topic, omit `message_thread_id`
- if the render target is a real topic, include the exact topic
  `message_thread_id`

### Topic Creation

`/new` requires Telegram `createForumTopic`.

Startup should verify topic capability before thread-native mode is used.

At minimum:

- call `getMe`
- require `has_topics_enabled = true`
- fail fast with a clear configuration error if the bot does not support topics
  in private chats

The implementation should treat `400`-class topic-creation failures such as
“chat is not a forum” or equivalent private-topic capability failures as
configuration errors, not as recoverable per-command runtime errors.

---

## Error Handling

If topic creation fails:

- `/new` returns a lobby error
- no thread record is persisted

If a topic is missing when sending:

- mark the record archived/detached
- surface a service message in the lobby

If `/resume <id>` targets a nonexistent ACP session:

- keep the current topic binding unchanged
- surface a topic-local error

If a callback arrives for a prompt whose thread was archived or rebound:

- answer it as stale
- do not mutate prompt or session state beyond normal stale cleanup

If a topic already has a live binding and `/resume <id>` is used:

- archive the old binding
- do not delete its ACP session record

---

## Testing Strategy

Minimum required coverage:

- router tests for `message_thread_id` extraction on topic messages and
  callbacks
- runtime tests for:
  - lobby plain text rejection
  - `/new` lobby behavior
  - per-topic `Unbound -> Active`
  - lazy `RestorePending -> ResumeSession -> Active`
  - topic-local `/resume <id>` rebinding
  - archived/detached topic behavior
  - prompt routing by `ThreadKey`
- persistence tests for registry round-trip and archived-thread retention
- transport tests for topic-targeted send/draft params

Manual smoke checklist:

1. enable topic mode for the bot
2. start `spur bot telegram`
3. create two topics via `/new`
4. start distinct SPUR sessions in both topics
5. verify output, reviews, and permissions stay topic-local
6. restart the bot
7. verify first post-restart message in a topic resumes lazily before sending
8. close one topic and confirm the binding remains archived in `/sessions`

---

## Consequences for Existing Code

The following existing simplifications are no longer valid:

- one global `current_acp_session_id`
- one global `current_brain`
- one global binding state
- one global prompt/render surface for the whole DM

Concretely, this impacts:

- [crates/spur-bot/src/state.rs](../../../crates/spur-bot/src/state.rs)
- [crates/spur-bot/src/runtime.rs](../../../crates/spur-bot/src/runtime.rs)
- [crates/spur-bot/src/telegram/router.rs](../../../crates/spur-bot/src/telegram/router.rs)
- [crates/spur-bot/src/telegram/client.rs](../../../crates/spur-bot/src/telegram/client.rs)
- [crates/spur-bot/src/telegram/render.rs](../../../crates/spur-bot/src/telegram/render.rs)
- [crates/spur-bot/src/telegram/mod.rs](../../../crates/spur-bot/src/telegram/mod.rs)
- [crates/spur-cli/src/commands/init.rs](../../../crates/spur-cli/src/commands/init.rs)

The new architecture is still intentionally Telegram-shaped. It should not be
forced into a premature cross-platform abstraction before this thread-native
model is proven in production.
