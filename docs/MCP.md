# Corcho MCP server: full control surface

A single Model Context Protocol server that exposes **every** Corcho capability
to an agent. The GUI and the MCP server share one Rust core when the in-app
endpoint is used; neither holds state the other cannot reach.

Transports implemented: **stdio** (`corcho --mcp`), **Streamable HTTP** served
by the running app on `http://127.0.0.1:7411/mcp`, and standalone Streamable
HTTP (`corcho --mcp-http 127.0.0.1:7411`). The HTTP endpoint uses a bearer token
stored at `~/.local/share/corcho/mcp.token`. Protocol version is negotiated at
initialize; tools, resources and prompts come from one registry.

---

## 1. Architecture

```
              ┌──────────────────────────┐
  GUI (Qt) ──▶│        Core (Rust)       │◀── MCP server
              │ model · storage · history│
              │ ai · sync · updates      │
              │ integrations             │
              └───────────┬──────────────┘
                          │ broadcast events (tokio::broadcast)
              ┌───────────┴──────────────┐
              │ AppController (Qt)  MCP  │
              └──────────────────────────┘
```

Rules:
- One `Arc<Mutex<Core>>` (already present) is the single source of truth.
- Every mutation goes through `Core` methods, so undo/redo, persistence and
  validation apply identically to GUI and MCP calls.
- The running GUI serves the Streamable HTTP endpoint on loopback, backed by
  its own `Arc<Mutex<Core>>`. MCP clients therefore mutate the live process:
  tool calls are serialized with Qt access through the same mutex, and the GUI
  repaints immediately after each call.
- The standalone server (`corcho --mcp`, `corcho --mcp-http`) runs **headless**
  against the same data directory. It reloads `workspace.json` when the file
  changes, while the GUI polls and reloads on external changes, so edits
  converge without a socket.
- No tool reads or writes files outside the configured data directory, except
  explicit import/export paths validated by a path guard.

---

## 2. Transports

| Mode | Command | Use |
| --- | --- | --- |
| in-app HTTP | running GUI, `http://127.0.0.1:7411/mcp` | opencode and other agents sharing the live app |
| stdio | `corcho --mcp` | Claude Desktop, standalone local agents |
| HTTP | `corcho --mcp-http 127.0.0.1:7411` | Standalone remote tooling, multiple clients |

The in-app endpoint is always on while the GUI runs unless `CORCHO_MCP_HTTP`
overrides the bind address. HTTP requires a bearer token written to
`~/.local/share/corcho/mcp.token` (mode 0600) and validates `Origin` against a
localhost allowlist. Bind is
loopback unless `--allow-remote` is explicit.

---

## 3. Tool catalogue

Every tool returns structured content plus a short text summary. Mutations
include `expected_revision` for optimistic concurrency; a mismatch returns
`REVISION_CONFLICT` with the current revision.

### 3.1 Workspace & grids

| Tool | Input | Effect |
| --- | --- | --- |
| `workspace.get` | - | Full workspace, revision, active grid |
| `workspace.revision` | - | Current revision only |
| `workspace.import` | `path`, `format` (`corcho`\|`legacy`), `dryRun` | Import, returns diff summary |
| `workspace.export` | `path`, `includeAttachments` | Write a backup zip |
| `workspace.reset` | `confirm` | Wipe all data (typed confirmation) |
| `grid.list` | `folderId?` | Grids with card counts |
| `grid.get` | `gridId` | Grid + cards + view state |
| `grid.create` | `name`, `folderId?` | New grid |
| `grid.rename` | `gridId`, `name` | Rename |
| `grid.setIcon` | `gridId`, `icon` (Lucide name or absolute `.svg` path) | Set grid icon |
| `grid.delete` | `gridId` | Delete (refuses last grid) |
| `grid.switch` | `gridId` | Change active grid |
| `grid.reorder` | `gridId`, `beforeId?` | Reorder |
| `grid.move` | `gridId`, `folderId?` | Move into/out of a folder |
| `grid.view` | `gridId`, `panX`, `panY`, `zoom` | Persist view |
| `folder.list/create/rename/delete/toggle` | as expected | Folder lifecycle |

### 3.2 Cards

| Tool | Input | Effect |
| --- | --- | --- |
| `card.list` | `gridId`, `kind?`, `query?` | Cards |
| `card.get` | `cardId` | Card + payload |
| `card.create` | `gridId`, `kind`, `x`, `y`, `title?`, `payload?` | Create |
| `file.pin` | `path`, `gridId?`, `x?`, `y?` | Pin image, Markdown or folder (PDFs open externally) |
| `card.update` | `cardId`, `patch` (geometry, title, text, color, icon, locked, payload) | Patch |
| `card.delete` | `cardId` | Delete |
| `card.duplicate` | `cardId`, `offset?` | Duplicate |
| `card.move` | `cardId`, `gridId`, `x`, `y` | Move across grids |
| `card.resize` | `cardId`, `width`, `height` | Resize |
| `card.setColor` | `cardId`, hex | Color |
| `card.setIcon` | `cardId`, lucide name | Icon |
| `card.lock` | `cardId`, boolean | Lock |
| `card.search` | `query`, `limit?` | Ranked results with snippets |

Kind-specific tools:

| Tool | Purpose |
| --- | --- |
| `note.append` / `note.replace` | Edit note text |
| `checklist.add/update/remove/toggle` | Checklist items |
| `kanban.addColumn/renameColumn/removeColumn/addItem/moveItem/removeItem` | Kanban |
| `whiteboard.clear`, `whiteboard.stroke` | Stroke list |
| `pomodoro.configure`, `pomodoro.reset` | Timer configuration and reset |
| `web.state`, `web.navigate` | Read or set a web card URL (the GUI navigates on reload) |
| `web.fetch`, `web.dom` | Fetch HTML or render the DOM in headless Chromium |
| `web.screenshot` | Render the page in headless Chromium; returns a text summary plus an MCP image content block |
| `api.fetch` | Run an API Data card |
| `script.run` | Execute a card script in `/bin/sh` (permission-gated) |
| `mpris.list/action/metadata` | Media control |
| `portal.set` | Target grid |
| `calendar.*` | Calendars, date links, reminders and agenda |
| `timbre.configure/state` | Timbre audio settings |

### 3.2.1 Calendar

| Tool | Input | Effect |
| --- | --- | --- |
| `calendar.list` | - | Calendars and resolved date links |
| `calendar.create` | `name`, `color?` | Create a calendar |
| `calendar.update` | `calendarId`, `name?`, `color?` | Rename or recolour |
| `calendar.delete` | `calendarId`, `confirm: "yes"` | Delete a calendar and its links |
| `calendar.link.add` | `date`, `calendarIds[]`, `title?`, `sourceCardId?`, `sourceItemId?` | Link a date |
| `calendar.link.update` | `linkId`, `date?`, `calendarIds?`, `title?` | Change a link |
| `calendar.link.delete` | `linkId` | Remove a link |
| `calendar.link.list` | `calendarIds?`, `from?`, `to?`, `sourceCardId?` | Filtered links |
| `calendar.agenda` | `from`, `to`, `calendarIds?` | Resolved agenda |
| `calendar.setMonth` | `cardId`, `year`, `month` (0-11) | Change a card's month |
| `calendar.today` | `cardId` | Return a card to the current month |

A date link can belong to multiple calendars. A Calendar card stores its own
`calendarIds` filter; an empty filter shows every calendar. Kanban and
checklist items use the same link model, so their dates appear in the agenda
and can be edited from either side.

### 3.2.2 Viewport and frame

| Tool | Input | Effect |
| --- | --- | --- |
| `view.get` | `gridId?` | Pan, zoom, viewport size and visible world frame |
| `view.set` / `grid.view` | `gridId?`, `panX?`, `panY?`, `zoom?`, `viewportWidth?`, `viewportHeight?` | Change view |
| `frame.get` | `gridId?`, `frame?`, `viewportWidth?`, `viewportHeight?` | Visible world rectangle |
| `card.visibility` | same as `frame.get` | Per-card `inside`, `intersects` and `visibleFraction` |

The GUI reports its canvas size through `AppController.setViewport`, so these
tools work against the real window once Corcho has been open. Pass an explicit
`frame` or viewport size when running headless.

### 3.3 AI

| Tool | Input | Effect |
| --- | --- | --- |
| `ai.providers` | - | Provider + model catalogue |
| `ai.validateKey` | `provider`, `apiKey` | Validate (never logged) |
| `ai.setKey` / `ai.clearKey` | `provider`, `apiKey?` | Store in local settings |
| `ai.setModel` | `provider`, `model` | Default model |
| `ai.complete` | `provider?`, `model?`, `prompt`, `context?`, `webSearch?` | Non-streaming completion |
| `ai.stream` (planned) | same, `streamId` | Emits `ai.chunk`/`ai.done` notifications |
| `ai.cancel` (planned) | `streamId` | Cancel |
| `ai.card.run` | `cardId` | Run an AI card with its saved prompt/context |
| `ai.models.local` | `serverUrl`, `apiKey?` | List local models |
| `ai.stats` | - | Card counts, token estimates |

API keys are write-only: `settings.get` redacts them; logs never include them.

### 3.4 Settings, themes & appearance

| Tool | Input |
| --- | --- |
| `settings.get` | `redactSecrets?` |
| `settings.set` | `key`, `value` (validated against the schema) |
| `settings.batch` | `patch` object, atomic |
| `theme.list` | - |
| `theme.get` | Current mode, light slot, dark slot and manual effective theme |
| `theme.set` | `themeId`, optional `mode` (`light`\|`dark`; omit to set both) |
| `theme.mode.set` | `system`\|`light`\|`dark` |
| `theme.custom.set` | `background`, `surface`, `accent`, `text` |
| `theme.css.template` | Return the complete CSS theme template |
| `theme.css.get` | Read a CSS theme |
| `theme.css.set` | `themeId`, `css`; validate and write a CSS theme |
| `theme.css.delete` | `themeId`; remove a user CSS theme |
| `font.list` / `font.set` | Font id |
| `effects.set` | spawn, parallax, ripple, reducedMotion |
| `audio.setVolume` | 0–100 |

### 3.5 Sync, backups, data

| Tool | Input |
| --- | --- |
| `sync.status` | - |
| `sync.now` | - |
| `sync.configure` | `enabled`, `provider` (`folder`\|`gdrive`), `folder?` |
| `gdrive.connect` / `gdrive.disconnect` | PKCE flow (browser) |
| `iroh.status` | enabled, namespace, endpoint id, relay, encryption, sync fingerprint, peers, last sync, last error |
| `iroh.create` | create an encrypted shared workspace and return its write setup code |
| `iroh.join` | `ticket` (a raw ticket or a Corcho setup code) |
| `iroh.share` | `readOnly` (`false` returns the write setup code) |
| `iroh.cache` | read-only cache ticket without the encryption key |
| `iroh.pairCreate` | upload the setup code and return a six-word pairing code |
| `iroh.pairJoin` | `code` |
| `iroh.sync` | publish local changes and pull remote ones |
| `iroh.leave` | leave the shared workspace on this device |
| `iroh.peers` | connected peers |
| `backup.export` / `backup.import` | `path` |
| `data.setDir` | `path` |
| `attachments.list/read/write/delete` | base64 for binary |

### 3.6 Updates and OS

| Tool | Input |
| --- | --- |
| `update.check` | - |
| `update.channel.set` | `stable`\|`beta`\|`experimental` |
| `update.download` / `update.install` | - |
| `app.launchAtStartup` | boolean |
| `app.openExternal` | URL (allowlist http/https) |
| `app.quit` / `app.relaunch` | confirm |
| `app.changelog` | - |
| `app.about` | version, build info |
| `diagnostics.run` | integrity checks |

### 3.7 Launcher, tutorials, easter eggs

| Tool | Input |
| --- | --- |
| `launcher.search` | `query` |
| `launcher.run` | `commandId`, `args?` |
| `tutorial.reset` | - |
| `onboarding.complete` | - |

---

## 4. Resources

Stable URIs for read-only context:

```
corcho://workspace
corcho://grid/{id}
corcho://card/{id}
corcho://settings
corcho://themes
corcho://models
corcho://changelog
corcho://backup/latest
corcho://diagnostics
```

MIME types: `application/json` for structured documents, `text/markdown` for
changelog, `application/zip` for backups.

## 5. Prompts

Reusable agent prompts exposed by the server:

- `organize-inbox`: triage unlinked notes into a grid.
- `summarize-grid`: summarize a grid's cards.
- `weekly-review`: collect pomodoros, checklists and notes into a review.
- `research-to-cards`: run a web/AI research pass and create cards.
- `clean-duplicates`: find duplicate cards and propose merges.

Each prompt declares arguments (gridId, date range) and returns messages that
call the tools above.

## 6. Events / subscriptions (planned)

The server broadcasts JSON-RPC notifications:

```
card.created      { gridId, card }
card.updated      { gridId, card, changed }
card.deleted      { gridId, cardId }
grid.changed      { grid }
workspace.saved   { revision }
sync.status       { state }
update.status     { state }
ai.chunk          { streamId, text }
ai.done           { streamId, text }
script.output     { cardId, chunk }
```

Clients may subscribe to a subset with `resources/subscribe` or the custom
`events.subscribe` tool. Notifications are coalesced at 50 ms to avoid floods
during drags.

## 7. Safety model

Implemented:

- **Write scope:** imports and exports require absolute paths; attachments are
  reduced to their file name before joining the attachments directory;
  `update.install` only accepts files inside `updates/`.
- **Secrets:** `settings.get`, `workspace.get` and `corcho://workspace` redact
  API keys, OAuth tokens and client secrets; errors never echo keys.
- **Confirmation:** `workspace.reset`, `calendar.delete` and `app.relaunch`
  require `confirm: "yes"`; `quit` requires `confirm: true`.
- **Input validation:** card kinds, colours, icons, coordinates, sizes,
  frame rectangles and HTTP(S) URLs are validated. Unknown kinds, schemes
  other than http/https, embedded credentials and out-of-range geometry are
  rejected before any mutation.
- **Size caps:** stdio lines and HTTP bodies are capped at 8 MiB, attachment
  base64 at 32 MB per call, `web.fetch` at 2 MB of text, and script bodies at
  100 kB.
- **Scripts:** `script.run` is disabled until `settings.set` sets
  `allow_scripts` to true; timeouts are clamped to 1-300 seconds.
- **Concurrency:** `expectedRevision` rejects stale mutations with
  `REVISION_CONFLICT`.
- **Explicit network:** web, AI, sync and update tools only touch the network
  when called.

Planned:

- Unix-socket auth for a live GUI, per-tool rate limits, an audit log, and a
  script permission gate.

## 8. Implementation

- One newline-delimited JSON-RPC loop in `src/mcp.rs` serves both stdio and a
  minimal HTTP transport. The same `call_tool` registry backs both.
- `Core` owns the model, storage, history, sync, updates and integrations.
  The in-app HTTP endpoint and the GUI share the same `Arc<Mutex<Core>>`; the
  standalone stdio/HTTP modes hold their own `Core` and reload
  `workspace.json` when its mtime changes.
- The HTTP transport implements MCP Streamable HTTP: POST-only requests return
  `application/json` or an SSE stream (`text/event-stream`), notifications get
  an empty `202`, GET/DELETE return `405`, `MCP-Protocol-Version` is validated,
  and tool calls with a `progressToken` stream `notifications/progress`.
- `Workspace` carries a `revision` counter that increments on every save.
- Viewport size is reported by `CanvasView` through `AppController.setViewport`
  and persisted in the workspace, so frame math survives restarts.
- Web DOM and screenshots shell out to headless Chromium with a hard timeout.
- Tool schemas live next to their handlers in one registry. Every tool carries
  a title, a multi-line description, per-property titles and descriptions, and
  MCP annotations (`readOnlyHint`, `destructiveHint`, `idempotentHint`,
  `openWorldHint`).
- Tests cover registry advertisement, every callable tool (with network,
  destructive and exiting tools skipped), frame visibility, revision conflicts
  and missing arguments.

## 9. Why an MCP server

The canvas is a workspace for thinking. An agent that can read the workspace,
create and edit cards, run AI nodes, and trigger sync/update actions turns
Corcho into a programmable surface without scripting the GUI. Because every
capability already lives in `Core`, the MCP layer is a thin, auditable adapter,
not a second application.


---

## 10. Implementation status

Implemented in `src/mcp.rs`, started with `corcho --mcp` or `corcho --mcp-http`:

- Transports: newline-delimited JSON-RPC over stdio, and minimal HTTP with a
  bearer token and localhost Origin check.
- Methods: `initialize`, `ping`, `tools/list`, `tools/call`, `resources/list`,
  `resources/read`, `prompts/list`, `prompts/get`.
- Workspace/grid/folder: get, revision, export, import, reset, list, get,
  create, rename, delete, switch, reorder, move, view, and folder lifecycle.
- Cards: list (with grid/kind/query filters), get, create (any grid, optional
  payload and geometry), update, delete, duplicate (offset), move (in-grid or
  across grids), resize, setColor, setIcon, lock, search, visibility.
- Viewport: `view.get`, `view.set`, `grid.view`, `frame.get`,
  `card.visibility`.
- Kind-specific: note, checklist, kanban (columns and items), whiteboard,
  pomodoro, calendar, date links, timbre, API fetch, script run, grid portal,
  MPRIS.
- Web: `web.state`, `web.navigate`, `web.fetch`, `web.dom`, `web.screenshot`.
  DOM and screenshot rendering use headless Chromium. They render the stored
  URL, not the live QtWebEngine profile or its cookies.
- AI: providers, validateKey, setKey/clearKey, setModel, complete, models.local,
  card.run, stats.
- Grids: create, rename, switch, move, reorder, delete, custom SVG icons.
- Appearance: theme list/get/set/custom, CSS themes (template/get/set/delete), system/light/dark mode, font list/set, effects, audio volume.
- Sync/data: folder/Drive sync, Iroh status/create/join/share/sync/leave/peers, backup
  export/import, data.setDir, attachments list/read/write/delete.
- OS/app: update check/download/install, launch at startup, open external,
  relaunch, changelog, about, diagnostics, tutorial reset, onboarding complete,
  adblock check/hosts, quit.
- Resources: workspace, grid, card, settings, themes, models, diagnostics,
  changelog.
- Prompts: organize-inbox, summarize-grid, weekly-review, research-to-cards,
  clean-duplicates.
- Optimistic concurrency: any tool accepts `expectedRevision`; a stale value
  returns `REVISION_CONFLICT`.

### Surface analysis

Covered without a running GUI:

- All model, card, grid, folder, calendar, date link, settings, theme, font,
  backup, attachment, sync, Iroh peer sync, update, MPRIS, script and AI calls.
- Frame/visibility math against a supplied frame or a previously reported
  viewport.
- Web content for public URLs through headless Chromium.
- Calendar filtering and agenda resolution across grids.

Still requires the running GUI or a future socket transport:

- Capturing the live QtWebEngine view, its cookies, navigation history, or
  authenticated session state.
- Driving the visible window's transient UI state (open dialogs, selection,
  drag, launcher state).
- Google Drive OAuth (`gdrive.connect` reports a GUI-only error).
- Pomodoro start/pause and AI token streaming (configuration and non-streaming
  completion are available).

The registry, resources and prompts above are complete for the implemented
handlers. Previously several card tools existed in `call_tool` but were missing
from `tools/list`; that gap is fixed and covered by a test.
