# SPUR TUI Architecture

> Reviewed 2026-04-16. Covers `spur-tui` (15.7k LOC) and its interface with `spur-acp` (6.4k LOC).

## 1. System Context

The TUI is the user-facing presentation layer. It consumes events from the orchestrator, renders them as a terminal interface, and sends user commands back.

```mermaid
flowchart TB
    USER((User))

    subgraph SPUR["SPUR Process"]
        subgraph TUI["spur-tui (15.7k LOC)"]
            APP["App\n(event loop)"]
        end
        subgraph CORE["spur-core"]
            ORCH["Orchestrator"]
            LINEAGE["ExecutorLineage\n(event-sourced)"]
        end
        subgraph ACP["spur-acp"]
            ADAPTER["Adapter Layer\n(presentation contract)"]
            CONN["Connection Layer\n(ACP transports)"]
        end
    end

    AGENTS["External Agents\n(Claude, Codex, Kiro)"]

    USER -->|"keyboard"| APP
    APP -->|"mpsc(UserInput)"| ORCH
    ORCH -->|"broadcast(SpurEvent)"| APP
    ORCH -->|"broadcast(SpurEvent)"| LINEAGE
    APP -.->|"reads &ExecutorLineage\nvia ViewContext"| LINEAGE
    ORCH <-->|"ACP JSON-RPC"| CONN
    CONN <-->|"stdio"| AGENTS
    CONN -->|"notifications"| ADAPTER
    ADAPTER -->|"ToolFamily\nToolInputDisplay\nObservePayload"| APP

    style TUI fill:#1a1a2e,stroke:#e94560,color:#fff
    style CORE fill:#1a1a2e,stroke:#0f3460,color:#fff
    style ACP fill:#1a1a2e,stroke:#533483,color:#fff
```

---

## 2. Internal Layer Architecture

Four layers with strict downward dependencies. No upward calls — only `Action` values bubble up via return.

```mermaid
flowchart TB
    subgraph L1["Layer 1 — App Coordinator (1702 LOC)"]
        APP_LOOP["Event Loop\n(tokio select!)"]
        APP_DISPATCH["View Dispatch\n(match current_view)"]
        APP_ACTION["Action Executor\n(match action)"]
        APP_STATE["Owned State\n• lineage: ExecutorLineage\n• brain_status: BrainStatus\n• metadata_store\n• config: Arc·SpurConfig·"]
    end

    subgraph L2["Layer 2 — Views (4345 LOC)"]
        DASH["DashboardView\n1098 LOC\n─────────\nagents tree\nactivity log\ndetail pane\ninput bar"]
        DETAIL["SessionDetailView\n2136 LOC\n─────────\nreact trace\ninput bar\nworkers panel\nstatus bar"]
        PICKER["SessionPickerView\n1111 LOC\n─────────\nsession list\nsearch / filter\nrename / archive\nconfirm-switch"]
        MERMAID["MermaidViewer\n(overlay)"]
    end

    subgraph L3["Layer 3 — Components (8000+ LOC)"]
        RT["ReactTrace\n3452 LOC\n(mod/builder/\nrender/types)"]
        IB["InputBar\n1038 LOC\n(vim/history/\ncomplete/paste)"]
        MS["MarkdownStream\n741 LOC"]
        TF["TraceFormat\n445 LOC"]
        LW["LineWrap\n402 LOC"]
        DP["DetailPane\n350 LOC"]
        MM["Mermaid\n346 LOC"]
        IEC["InlineExecutor\nCard 313 LOC"]
        AT["AgentsTree\n289 LOC"]
        WP["WorkersPanel"]
        AL["ActivityLog"]
        SB["StatusBar"]
        RC["ReviewCard"]
        DV["DiffViewer"]
        HO["HelpOverlay"]
    end

    subgraph L4["Layer 4 — Support Modules"]
        INGEST["agents/ingest\n+ entry_builder\n(ACP→TraceEntry)"]
        CMD["commands/\nregistry\nsubmit_router"]
        MENTION["mentions/\nregistry\nfile_source"]
        META["session_metadata\n(drafts, history)"]
    end

    APP_LOOP --> APP_DISPATCH
    APP_DISPATCH --> DASH & DETAIL & PICKER & MERMAID
    DASH --> AT & AL & DP & IB & RC
    DETAIL --> RT & IB & WP & SB & IEC & MM
    PICKER --> IB
    RT --> TF & LW & MS
    DETAIL --> INGEST
    DASH --> INGEST
    DETAIL --> CMD & MENTION
    APP_LOOP --> META

    style L1 fill:#0d1117,stroke:#e94560,color:#fff
    style L2 fill:#0d1117,stroke:#0f3460,color:#fff
    style L3 fill:#0d1117,stroke:#533483,color:#fff
    style L4 fill:#0d1117,stroke:#16213e,color:#fff
```

---

## 3. Event & Data Flow

### 3a. Inbound: SpurEvent → Screen

```mermaid
sequenceDiagram
    participant O as Orchestrator
    participant BC as broadcast channel
    participant A as App (event loop)
    participant V as Active View
    participant RT as ReactTrace
    participant B as Builder
    participant R as Renderer

    O->>BC: emit(SpurEventBody::AgentNotification)
    BC->>A: recv() (≤64 events/frame)
    A->>A: update lineage projection
    A->>A: construct ViewContext { &lineage, &brain_status }
    A->>V: handle_spur_event(&event, &ctx)
    V->>V: ingest → TraceEntry (via agents/ingest)
    V->>RT: push(TraceEntry)
    RT->>RT: mark_dirty_from(idx), bump generation

    Note over A: next render frame (30fps)
    A->>A: construct ViewContext
    A->>V: render(frame, area, &ctx)
    V->>RT: render(&mut self, frame, area, lineage)
    RT->>B: build_display_lines() or build_virtual_rows()
    RT->>R: viewport slice → Paragraph → frame
```

### 3b. Outbound: Keypress → Orchestrator

```mermaid
sequenceDiagram
    participant U as User
    participant A as App
    participant V as Active View
    participant IB as InputBar
    participant O as Orchestrator

    U->>A: KeyEvent
    A->>A: construct ViewContext
    A->>V: handle_key(key, &ctx)
    V->>IB: route key (vim mode, completion, etc.)
    IB-->>V: Option·Action·
    V-->>A: Some(Action::SendMessage { text })
    A->>A: match action
    A->>O: user_input_tx.send(UserInput::Message(text))
```

### 3c. Review Loop

```mermaid
sequenceDiagram
    participant O as Orchestrator
    participant A as App
    participant D as DashboardView
    participant RC as ReviewCard
    participant U as User

    O->>A: SpurEvent(ExecutorReviewRequested)
    A->>A: lineage.apply(event) → node.phase = AwaitingReview
    A->>D: handle_spur_event → activity_log.push(review entry)

    Note over D: render frame
    D->>D: render detail_pane with ReviewCard
    RC->>RC: show [a]pprove [d]eny [m]odify [R]etry

    U->>A: KeyEvent('a')
    A->>D: handle_key('a', &ctx)
    D->>D: read attempt_n from ctx.lineage
    D-->>A: Some(Action::SubmitReview { decision: Approve, attempt_n })
    A->>O: user_input_tx.send(UserInput::ReviewDecision(...))
```

---

## 4. View Trait & ViewContext

```mermaid
classDiagram
    class View {
        <<trait>>
        +handle_key(key, ctx) Option~Action~
        +handle_spur_event(event, ctx)
        +render(frame, area, ctx)
        +tick()
    }

    class ViewContext {
        +lineage: &ExecutorLineage
        +brain_status: &BrainStatus
    }

    class App {
        -current_view: ViewId
        -dashboard: DashboardView
        -session_detail: Option~SessionDetailView~
        -session_picker: Option~SessionPickerView~
        -lineage: ExecutorLineage
        -brain_status: BrainStatus
        -metadata_store: SessionMetadataStore
        -config: Arc~SpurConfig~
        -user_input_tx: mpsc Sender
    }

    class DashboardView {
        -agents_tree: AgentsTree
        -activity_log: ActivityLog
        -detail_pane: DetailPane
        -input_bar: InputBar
    }

    class SessionDetailView {
        -react_trace: ReactTrace
        -input_bar: InputBar
        -mermaid_registry: HashMap
        -stream_in_flight: bool
        -cancel_mode: Option~CancelMode~
    }

    class SessionPickerView {
        -sessions: Vec~SessionInfo~
        -cursor: usize
        -filter: String
        -rename_state: Option
    }

    App --> View : dispatches to
    App --> ViewContext : constructs per-frame
    View <|.. DashboardView
    View <|.. SessionDetailView
    View <|.. SessionPickerView
    DashboardView --> AgentsTree
    DashboardView --> ActivityLog
    DashboardView --> DetailPane
    DashboardView --> InputBar
    SessionDetailView --> ReactTrace
    SessionDetailView --> InputBar
    SessionDetailView --> WorkersPanel
```

---

## 5. ACP Adapter → TUI Rendering Pipeline

The adapter layer in `spur-acp` creates a presentation contract that the TUI consumes without knowing which agent produced the data.

```mermaid
flowchart LR
    subgraph ACP_CONN["spur-acp / connection"]
        NATIVE["native.rs\n(ACP JSON-RPC)"]
        STDIO["stdio_adapter"]
        STREAM["stream_json"]
        CLIWRAP["cli_wrap"]
    end

    subgraph ACP_ADAPT["spur-acp / adapter"]
        CLAUDE["claude.rs"]
        CODEX["codex.rs"]
        KIRO["kiro.rs"]
        GENERIC["generic.rs"]
        CONTRACT["Presentation Contract\n─────────\nToolFamily (12 variants)\nToolInputDisplay (7 variants)\nObservePayload (6 variants)\nAgentKind (4 variants)"]
    end

    subgraph TUI_INGEST["spur-tui / agents"]
        INGEST["ingest.rs\n+ entry_builder.rs"]
        TRACE_ENTRY["TraceEntry\n{ kind: TraceKind,\n  text, timestamp,\n  markdown? }"]
    end

    subgraph TUI_RENDER["spur-tui / components"]
        TRACE_FMT["trace_format.rs\n(glyph, color, summary)"]
        BUILDER["builder.rs\n(display lines,\nvirtual rows)"]
        RENDER["render.rs\n(viewport, cache,\nscrollbar)"]
    end

    NATIVE --> CLAUDE & CODEX & KIRO & GENERIC
    STDIO --> GENERIC
    STREAM --> GENERIC
    CLIWRAP --> GENERIC
    CLAUDE & CODEX & KIRO & GENERIC --> CONTRACT
    CONTRACT --> INGEST
    INGEST --> TRACE_ENTRY
    TRACE_ENTRY --> BUILDER
    BUILDER --> TRACE_FMT
    BUILDER --> RENDER

    style CONTRACT fill:#e94560,stroke:#e94560,color:#fff
    style TRACE_ENTRY fill:#0f3460,stroke:#0f3460,color:#fff
```

### Adding a New Agent

| Step | File | Change |
|---|---|---|
| 1 | `spur-acp/src/adapter/gemini.rs` | New adapter: parse notifications → ToolFamily + ToolInputDisplay + ObservePayload |
| 2 | `spur-acp/src/types.rs` | Add `AgentKind::Gemini` variant |
| 3 | `spur-acp/src/connection/` | Transport support (if non-stdio) |
| 4 | `spur-tui/src/components/react_trace/mod.rs` | 1 line: accent color in `pane_title_and_color()` |
| — | All other TUI files | **Zero changes** |

---

## 6. ReactTrace Render Pipeline

The largest component (3452 LOC), decomposed into 4 submodules after P3.3a.

```mermaid
flowchart TB
    subgraph TYPES["types.rs (85 LOC)"]
        TK["TraceKind\n(7 variants)"]
        TE["TraceEntry\n{ kind, text,\ntimestamp, markdown? }"]
        VR["VirtualRow\n(Text | ImageRow)"]
        SEG["Segment\n(Text | Image)"]
    end

    subgraph MOD["mod.rs (1086 LOC)"]
        RT_STRUCT["ReactTrace struct\n• entries: Vec·TraceEntry·\n• scroll_offset, is_following\n• generation, dirty_from\n• line_cache"]
        RT_DATA["Data methods\nappend_think, append_message\npush, scroll_*, tick\ninvalidate_cache, mark_dirty_from"]
    end

    subgraph BUILDER["builder.rs (811 LOC)"]
        BDL["build_display_lines()\n→ Vec·Line· (pre-wrap)"]
        BVR["build_virtual_rows()\n→ (Vec·VirtualRow·, starts)"]
    end

    subgraph RENDER["render.rs (507 LOC)"]
        CACHE["Cache management\n• LineCacheEntry (non-md)\n• VirtualRowCacheEntry (md)\n• incremental rebuild"]
        REND["render(&mut self)\n• viewport slice\n• Paragraph widget\n• Scrollbar"]
        RCTX["render_with_ctx(&mut self)\n• segment_visible_rows()\n• Text → Paragraph\n• Image → StatefulImage"]
    end

    TE --> RT_STRUCT
    RT_DATA --> BDL & BVR
    BDL --> CACHE
    BVR --> CACHE
    CACHE --> REND & RCTX

    style TYPES fill:#16213e,stroke:#e94560,color:#fff
    style MOD fill:#16213e,stroke:#0f3460,color:#fff
    style BUILDER fill:#16213e,stroke:#533483,color:#fff
    style RENDER fill:#16213e,stroke:#e94560,color:#fff
```

### Cache Invalidation Strategy

```mermaid
stateDiagram-v2
    [*] --> Clean: render completes

    Clean --> DirtyTail: mark_dirty_from(idx)\n(append, tick, stream chunk)
    Clean --> DirtyFull: invalidate_cache()\n(toggle collapse, resize, mode change)

    DirtyTail --> IncrementalRebuild: next render\n(truncate rows[idx..],\nrebuild tail only)
    DirtyFull --> FullRebuild: next render\n(rebuild all rows)

    IncrementalRebuild --> Clean: cache.generation = self.generation
    FullRebuild --> Clean: cache.generation = self.generation

    Clean --> FullRebuild: width changed\nor fence_gen changed
```

---

## 7. State Ownership

```mermaid
flowchart TB
    subgraph APP["App (owns)"]
        LINEAGE["ExecutorLineage\n(event-sourced projection)"]
        BRAIN_ST["BrainStatus\n(Idle | Thinking | Error)"]
        META["SessionMetadataStore\n(drafts, history, pins)"]
        CONFIG["Arc·SpurConfig·"]
        EDIT_MODE["EditMode\n(Insert | Vim)"]
    end

    subgraph VCTX["ViewContext (borrows per-frame)"]
        VLIN["&lineage"]
        VBS["&brain_status"]
    end

    subgraph DASH_STATE["DashboardView (owns)"]
        D_TREE["AgentsTree state"]
        D_LOG["ActivityLog entries"]
        D_PANE["DetailPane tab + scroll"]
        D_INPUT["InputBar state"]
        D_FOCUS["focused_panel, focused_node"]
    end

    subgraph DETAIL_STATE["SessionDetailView (owns)"]
        S_TRACE["ReactTrace\n(entries, cache, scroll)"]
        S_INPUT["InputBar state"]
        S_MERMAID["mermaid_registry"]
        S_STREAM["stream_in_flight\ncancelling_in_flight\ncancel_mode"]
        S_COST["cost, context_used"]
    end

    APP --> VCTX
    VCTX --> DASH_STATE
    VCTX --> DETAIL_STATE

    LINEAGE -.->|"push: handle_spur_event"| DASH_STATE
    LINEAGE -.->|"pull: render reads &lineage"| DASH_STATE
    LINEAGE -.->|"push: handle_spur_event"| DETAIL_STATE
    LINEAGE -.->|"pull: render reads &lineage"| DETAIL_STATE

    style APP fill:#1a1a2e,stroke:#e94560,color:#fff
    style VCTX fill:#1a1a2e,stroke:#0f3460,color:#fff
```

**Push vs Pull:**
- **Push**: App calls `view.handle_spur_event()` — views update their own state (activity log entries, trace entries, stream flags)
- **Pull**: Views read `ctx.lineage` during `render()` — for live worker counts, review status, executor phases

---

## 8. File Map & LOC

| Layer | File | LOC | Responsibility |
|---|---|---|---|
| **App** | `app.rs` | 1702 | Event loop, view dispatch, action execution, state ownership |
| **Views** | `views/session_detail.rs` | 2136 | Agent conversation: trace + input + workers + status |
| | `views/session_picker.rs` | 1111 | Session list: search, rename, archive, confirm-switch |
| | `views/dashboard.rs` | 1098 | Multi-panel overview: tree, log, detail, input |
| | `views/mermaid_viewer.rs` | ~100 | Diagram overlay (delegates to app for rendering) |
| | `views/mod.rs` | ~120 | View trait, ViewContext, ViewId, macOS key normalization |
| **Components** | `react_trace/mod.rs` | 1086 | ReactTrace struct, data methods, tests |
| | `react_trace/builder.rs` | 811 | Display line / virtual row construction |
| | `react_trace/render.rs` | 517 | Viewport rendering, cache, scrollbar |
| | `react_trace/types.rs` | 85 | TraceKind, TraceEntry, VirtualRow, Segment |
| | `input_bar.rs` | 1038 | Vim mode, history, completion, bracketed paste |
| | `markdown_stream.rs` | 741 | Streaming markdown parser, mermaid fence detection |
| | `trace_format.rs` | 445 | Glyph, color, summary for tool calls |
| | `line_wrap.rs` | 402 | Unicode-aware line wrapping |
| | `detail_pane.rs` | 350 | Worker stream tabs (Thought/Message/Tool) |
| | `mermaid.rs` | 346 | Mermaid state machine (Pending→Rendering→Ready) |
| | `inline_executor_card.rs` | 313 | Embedded worker status cards |
| | `agents_tree.rs` | 289 | Tree widget for agent hierarchy |
| | Others (9 files) | ~600 | ActivityLog, StatusBar, HelpOverlay, ReviewCard, DiffViewer, CompletionPopup, etc. |
| **Support** | `agents/ingest.rs` | ~150 | ACP notification → TraceEntry translation |
| | `agents/entry_builder.rs` | 209 | Structured TraceEntry construction |
| | `commands/registry.rs` | 297 | Slash command registration + dispatch |
| | `commands/submit_router.rs` | 248 | Input submission routing (message vs command) |
| | `session_metadata.rs` | 248 | Persistent session state (JSON) |
| | `mentions/` | ~100 | @-mention file source |

---

## 9. Architectural Assessment

### Strengths

- **Clean 4-layer architecture** — App → Views → Components → Support with strict downward deps
- **View trait + ViewContext** — minimal, correct for ratatui's immediate-mode model
- **Adapter as presentation contract** — TUI is agent-agnostic (8/10 score); new agent = 1 TUI line
- **Event-sourced lineage** — pure projection, hybrid push/pull state flow
- **ReactTrace decomposition** — Model (mod) / ViewModel (builder) / View (render) with incremental caching
- **Ingest layer** — proper isolation of ACP protocol details from rendering

### Known Risks

| Risk | Severity | Mitigation |
|---|---|---|
| `broadcast::Lagged` drops events → stale lineage | **High** | Not implemented — replay from NDJSON needed |
| `app.rs` God Object (11 responsibilities, 1702 LOC) | Medium | Action execution extractable (~300 LOC) |
| Fire-and-forget `tokio::spawn` — no graceful shutdown | Medium | TaskTracker needed |
| `session_detail.rs` at 2136 LOC | Low | Cohesive — heavy components already extracted |

### Recommendations (prioritized)

1. **Implement Lagged recovery** — replay NDJSON to rebuild lineage (reliability)
2. **Extract ActionExecutor** from app.rs — ~300 LOC, improves testability
3. **Add TaskTracker** for JoinHandle management — graceful shutdown
4. **Do not split** session_detail.rs — it's large but cohesive
5. **Do not move** adapter types out of spur-acp — the presentation contract belongs there
