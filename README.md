# Corcho

A native desktop workspace where notes, sites, tasks and calendars share
one infinite grid. Built with **Rust** and **Qt 6** (Qt Quick and QtWebEngine),
for Linux. No Electron shell, no telemetry, no analytics.

![Corcho canvas](docs/screenshots/canvas.png)

![Corcho settings](docs/screenshots/settings.png)

## Features

- Infinite grid with pan, zoom, snapping, a minimap and undo/redo.
- Fifteen card types: note, website, bookmark, API data, AI node, script,
  calendar, timbre, grid portal, checklist, kanban, whiteboard, pomodoro,
  now playing and pinned files/folders.
- Multi-calendar planning: add reminders, link dates from kanban, checklists
  and any other card, then filter the calendar view per calendar.
- A dedicated Timbre card for noise and pure-tone audio, with volume, pitch
  and mix controls.
- Pin images, Markdown files and folders as cards; PDFs open in your system viewer.
- Custom SVG icons for every grid, plus a guided peer sync setup wizard.
- Themes follow the system light/dark appearance, with a separate theme chosen
  for light and dark.
- Five AI providers (Anthropic, OpenAI, Google, xAI and Mistral) plus a local
  OpenAI-compatible server, with streaming and context injection.
- Isolated website sessions, a host-based ad blocker and a cross-origin
  session probe.
- **Peer-to-peer sync** with Iroh: create a shared workspace, join it from
  another device with a ticket, and edits replicate directly, encrypted. No
  server or Git remote is required. Deletions use tombstones, losing values
  stay in `backups/`, attachments travel as content-addressed blobs, and an
  optional read-only `--sync-daemon` keeps an always-on copy on a NAS or home
  server. Folder and Google Drive sync remain available.
- Picture-in-picture cards: pop out one or more selected cards in their own
  always-on-top windows, keep working on them, and return them to the canvas
  from the greyed-out placeholder left behind.
- Stable, beta and experimental update channels, MPRIS, scripts, launch at
  startup and a `corcho://` protocol handler. Every feature is free.
- A complete **MCP server** over stdio or local HTTP: workspace, grids,
  folders, every card type, calendars and date links, viewport and frame
  queries, card visibility, settings, AI, web fetch/DOM/screenshot, Iroh sync,
  backups and updates. See [`docs/MCP.md`](docs/MCP.md).

## Palette

Two base neutrals and one accent, following the 60-30-10 rule.

| Role | Color | Share |
| --- | --- | --- |
| Base 1: ink (canvas, chrome) | `#121417` | 60% |
| Base 2: paper (text, light surfaces) | `#ECE7DE` | 30% |
| Accent: ember (actions, selection) | `#D98E5C` | 10% |

Surfaces derive from the ink base (`#1B1E23`, `#272B31`). Borders use the
paper tone at 9% alpha, and semantic colours are muted (success `#7FAE83`,
danger `#D97A72`). The accent is reserved for the primary action, the current
selection and one highlighted figure.

The 60-30-10 rule and the "two neutrals plus one accent" guidance come from
[Color Scheme Calculator](https://colorschemecalculator.com/create-color-palette/)
and [Sky Rye Design](https://skyryedesign.com/inspiration/color-palette-inspiration/).

## Accessibility

Contrast targets, focus behaviour and reduced-motion rules are documented in
[docs/ACCESSIBILITY.md](docs/ACCESSIBILITY.md).

## Build

Requires Rust 1.85 or newer, Qt 6.8+ development packages (Core, Gui, Qml,
Quick, QuickControls2, Svg, WebEngine and Network) and ALSA headers.

```sh
QMAKE=/usr/bin/qmake6 cargo build
QMAKE=/usr/bin/qmake6 cargo run
```

The build is Cargo-only; CMake is not required.

Run the checks:

```sh
cargo fmt --check
QMAKE=/usr/bin/qmake6 cargo clippy --all-targets -- -D warnings
QMAKE=/usr/bin/qmake6 cargo test
```

The release binary is about 11 MB. Full implementation status, including known
limits, is in [PLAN.md](PLAN.md).

## Install

The build is a single binary plus a desktop entry and icon. To install for the
current user:

```sh
install -m755 target/release/corcho ~/.local/bin/corcho
install -m644 packaging/corcho.desktop ~/.local/share/applications/
install -m644 packaging/corcho.svg ~/.local/share/icons/hicolor/scalable/apps/
update-desktop-database ~/.local/share/applications
xdg-mime default corcho.desktop x-scheme-handler/corcho
```

Packaging scripts for Debian, AppImage and Flatpak live in
[`packaging/`](packaging/).

## Picture-in-picture and compositor rules

Right-click a card (or a multi-selection) and choose **Open in
picture-in-picture**. Each card opens in its own frameless window, placed side
by side with a constant gap, that can be moved by its title bar; use the pin
button to keep it on top. The original card stays on the canvas as a grey
placeholder. Hover it and press **Back card to canvas** to close the detached
window.

The process advertises a stable Wayland `app_id` / X11 `WM_CLASS`:

- native and distribution packages: `corcho`
- Flatpak: `io.corcho.Corcho`

Picture-in-picture windows keep `Picture-in-Picture` in their title. On sway,
Corcho asks the compositor to float and move each detached window so the
constant-gap row is preserved. On other compositors that expose window
placement, the same client-side layout is used. Compositor rules can still
float or pin them, for example:

```ini
# Sway / i3
for_window [app_id="corcho" title="^Picture-in-Picture"] floating enable, sticky enable
```

```ini
# Hyprland
windowrulev2 = float, class:^(corcho)$, title:^(Picture-in-Picture)
windowrulev2 = pin, class:^(corcho)$, title:^(Picture-in-Picture)
```

## MCP

```sh
corcho --mcp                              # stdio
corcho --mcp-http 127.0.0.1:7411          # local HTTP with a bearer token
```

The HTTP token is written to `~/.local/share/corcho/mcp.token` with mode 0600.
Tools, resources and prompts are listed in [docs/MCP.md](docs/MCP.md).

## Data

`~/.local/share/corcho/workspace.json` holds the workspace. The
`attachments/`, `backups/`, `iroh/` and `updates/` directories sit beside it.
`iroh/` holds the endpoint key, document store, blob store and sync state; all
of it is local and mode 0600 where it holds secrets. Daily snapshot zips are
written to `backups/` and pruned to the newest eight.

### Peer sync daemon

For a durable always-on copy, join the workspace read-only from a NAS or home
server:

```sh
corcho --sync-daemon --ticket <read-ticket>   --data-dir ~/.local/share/corcho
```

The daemon stores every replicated record and attachment but never writes, so
a new device can sync while all your writer devices are offline. A systemd
unit example lives in `packaging/corcho-syncd.service`. Relays only provide
connectivity; they are not a durable copy. Add `--ttl-hours 48` to make the
daemon a rolling cache: it drops its document and blob stores when the timer
expires and the supervisor restarts it. Give the cache a **cache ticket**
(the wizard's *Copy cache ticket* button), which is read-only and omits the
workspace key, so the cache only ever holds ciphertext.

Workspaces created by this version use end-to-end encryption: record values
and attachments are sealed with a per-workspace key before they are published.
The setup code carries that key, so treat it like a password. A cache ticket
does not.

### Relay

Every device can share one redundant relay list instead of the public n0
relays. `CORCHO_IROH_RELAY` (or the generic `IROH_RELAY_URL`) takes a
comma-separated list; when both are unset, `~/.config/iroh/relay-url` is read
(one URL per line, `#` comments allowed):

```sh
CORCHO_IROH_RELAY=https://relay.example.com
```

An unreadable file or a non-empty setting with no valid URL fails startup
rather than silently falling back to n0. The Peer sync wizard also has a
**Relay endpoint** field: saving there writes `~/.config/iroh/relay-url` and
restarts the endpoint immediately; an empty value restores the n0 relays. The
same wizard shows this device's endpoint ID with a copy button. A private relay
can accept **Request access** from the wizard: the request is stored on the
server and the operator approves it out of band, so devices cannot add
themselves. Self-hosted relays can also restrict access to a list of endpoint
IDs; each device's `iroh.status` reports its `endpointId`, and `corcho
--print-endpoint-id [--data-dir PATH]` prints it without starting the app.


## Acknowledgements

Corcho is an independent native implementation inspired by the ideas in
[Spacesuit](https://spacesuit.lightmode.io/). It is not affiliated with or
endorsed by Lightmode Laboratories, and it shares no code or branding with
that project.

The interface polish follows the [unslop](https://github.com/heydamir/unslop)
method by Damir Sokolovsky ([@heydamir](https://x.com/heydamir)), licensed
under CC BY 4.0.

## License

MIT. See [LICENSE](LICENSE). Bundled Inter fonts are under the SIL Open Font
License; Lucide icons are ISC.

Branding uses TeX Gyre Adventor Bold (GUST Font License).
