# Herdr Agent Picker

**[Download v0.1.0](https://github.com/kungbi/herdr-agent-picker/releases/tag/v0.1.0)** · Native Rust plugin for macOS and Linux.

A compact, keyboard-driven agent popup for Herdr.

![Release Rust picker with Done, Active and date-grouped synthetic agents beside a colored terminal preview](docs/images/agent-picker-preview.png)

*Terminal preview: `j`/`k` to move, `/` to search, `p` to toggle preview, Enter to focus.*

![Release Rust picker with its centered alarm preset dialog over the synthetic agent list and preview](docs/images/agent-picker-alarm.png)

*Alarm presets: `a` to open, `j`/`k` or arrows to move, `1`–`8` or Enter to choose, Esc to cancel.*

Both screenshots capture the release Rust binary and real fzf in an isolated PTY,
rendered from ANSI with a monospace font. All agents, tasks, paths and preview
output are synthetic; no live Herdr sessions or notifications were used.

- Colored status first, bold task title, dim workspace/tab location.
- `j`/`k` or arrow keys to navigate; `Ctrl+D`/`Ctrl+U` to move five agents at a time.
  Enter to focus; Esc or `q` to close in normal mode.
- Press `/` to search by task, workspace, tab, or agent name. Use arrow keys to
  navigate results and Enter to focus. Esc clears the search and returns to normal
  navigation without closing the picker, even with an empty query or no matches.
  Press Esc or `q` in normal mode to close; while searching, `q` is ordinary text.
- Preview opens automatically on the right, using 60% of the popup width.
  Codex, Hermes and other agents use the same colored terminal snapshot path:
  their own rendered output, not a second Markdown renderer or rebuilt chat.
  Press `p` outside search to hide or show it.
  Preview may contain sensitive terminal text; hide it before screen sharing.
- Press `1`–`9` to focus a numbered item immediately, without Enter.
- Current Done and Active agents come first, followed by idle completion history by date.
- Completion ages such as `3m ago`; model metadata is hidden.
- Refreshes the list and status every second while the popup is open.
- Per-agent reminders with time presets, custom dates and cancellation.

## Requirements

- Herdr 0.9.1 or newer.
- System SQLite 3 library (included in macOS; `libsqlite3-0` on Debian/Ubuntu).
- fzf 0.73.1 or newer, available on PATH.
- macOS or Linux. Tested on macOS; Windows is not supported.

On macOS with Homebrew:

```sh
brew install fzf
```

Native release packages include the Rust executable and need no Python or Rust
installation. Use a package matching your OS and architecture; CI builds packages
for its macOS and Linux runner architectures. Other macOS/Linux architectures can build from
source. Linux GNU packages require a compatible glibc (build on the oldest target
system when distributing to older Linux releases).

## Build from source

A source checkout or source archive requires a Rust toolchain and SQLite headers
(`libsqlite3-dev` and a C toolchain on Debian/Ubuntu; Xcode command line tools on
macOS). Build within the checkout:

```sh
sh ./run build
sh ./run check
```

For a cached, disconnected build use `sh ./run build --offline`. The launcher uses
`bin/herdr-agent-picker` in native packages, or `target/release/herdr-agent-picker`
in a source checkout. It does not invoke Python, install anything globally, or
compile during hooks/popup startup. Rebuild after changing Rust source.

## Native packages

Extract the matching `.tar.gz` or `.zip` package to a permanent directory and
follow Local installation below. Check it against its `SHA256SUMS-<target>` file.
Source archives include Rust code plus Python build/test tooling, without a
prebuilt binary. `herdr plugin install` from a source Git ref also requires the
build step; automatic source compilation during installation is not assumed.

## Local installation

Keep this directory in a permanent location. Run:

```sh
sh ./run check
herdr plugin link "$PWD" --enabled
```

Add this to your Herdr `config.toml`, replacing any existing `prefix+g` command:

```toml
[keys]
goto = ""
next_agent = ""

[[keys.command]]
key = "prefix+g"
type = "plugin_action"
command = "agent-picker.open"
description = "Select agent"
```

Merge the entries into any existing `[keys]` section instead of duplicating it.
Then run `herdr server reload-config`. Use your prefix followed by `g`.

Native packages need no compilation. Build source checkouts before linking them.

## Original terminal preview

Every agent uses `herdr pane read --source recent-unwrapped --lines 30 --format ansi`.
The picker preserves the agent's rendered colors, emphasis, code, text and message
order instead of parsing Markdown again. There is no Hermes-specific database
lookup, saved-conversation header, role reconstruction, or newest-first reversal.

Right-edge frames are detected from a blank framed row and their terminal column,
counting Korean/wide and combining characters rather than string length. The
matching outer `│`/`┃` and trailing spaces are removed even with zero or short
padding; ordinary table borders and leading code indentation stay intact. ANSI
styles, original paragraph breaks and source-side hard line breaks are preserved.
The terminal you opened the picker from is previewed without resizing it. For
other agents, the picker temporarily attaches at the preview's rows and columns.
This resizes the selected terminal's PTY, not the Herdr split. The
attachment is released when selection changes, the preview is hidden, the picker
closes, or a read fails. The source application may redraw while attached and
again when released. A recent snapshot is not a pixel-identical mirror: the last
30 source rows can include input/status UI. A snapshot without a blank frame
reference, complex joined emoji widths, hard-drawn tables and source-side
truncation are not reconstructed. Literal Markdown already present in the source
is intentionally unchanged.

The visible preview refreshes every second, including when agent status is
unchanged. Hiding it stops pane reads. Attach is exclusive and never uses
`--takeover`; if another client holds the terminal, or attach/read fails, the
preview shows `Preview unavailable`. Enter still opens the original pane.
Previewing never sends input, changes focus, resizes splits, or acknowledges a
reminder. The baseline attachment protocol was checked with Herdr 0.9.5; this Rust port
validates it with isolated fake attachments. Glow is not used by the picker.

## Reminders

Select an agent and press `a` to schedule a reminder to revisit it, regardless of
whether its work has finished. A small centered dialog opens over the agent list;
Esc returns to the same selection. Custom input and the `r` reminder manager also
use compact dialogs. Choose 5, 10, 15 or 30 minutes, 1 or 2 hours,
tomorrow at 09:00, or **Custom time**.

Custom time has separate **Date** (year, month, day) and **Time** (hour, minute)
fields, prefilled to 30 minutes from now or the agent's existing reservation.

- `Tab` or `←`/`→`: select a field; brackets mark the active field.
- `↑`/`↓` or `k`/`j`: increase or decrease its value.
- Type digits to replace the selected field.
- `Enter`: save; `Esc`: cancel.

Dates and clock times use your computer's local timezone. Invalid dates and past
times show an error without changing the reservation.

Scheduled reminders appear beside their agent as `[alarm MM-DD HH:MM]`.
Unread reminders show a red `[!]` immediately after the number, so the marker
stays visible when the preview narrows the list. Opening the agent with Enter
clears the marker; closing the picker without opening it leaves the marker.

There is one reminder per agent: setting another replaces it. Press `a` again
and choose **Cancel current alarm**, or press `r` to list scheduled alarms and
select one to cancel. Escape leaves a menu without changing the reservation or
focus; Escape also closes the custom date/time form.

Reminders persist locally and run after the picker closes. At the due time,
Herdr displays a notification with its completion sound. A desktop notification
is also requested on macOS and on Linux when `notify-send` is installed. Allow
notifications in your desktop settings; the OS may suppress them in Focus mode.
The reminder never sends input to an agent or changes the focused pane.

An overdue reminder is delivered after the computer wakes. After a reboot, open
the picker to resume saved reminders. Failed deliveries are retried every minute.
Reminder data stays in `HERDR_PLUGIN_STATE_DIR/reminders.sqlite3` (or the standalone
state directory below); it is excluded from Git and release archives.

## Completion times and ordering

Herdr event hooks record when completion is observed. Tasks completed before the
plugin was enabled have no known completion time. This is not an archived task
history: the picker lists current agents and their latest observed completion.

Current Done agents come first, followed by Active. Within Active, when
`ui.agent_panel_sort = "priority"`, blocked agents precede working agents.
Done includes Herdr's `done` agents and observed working/blocked-to-idle finishes
that have not been visited since completion. Herdr can report a finish in its
last-focused pane as `idle` while the user is elsewhere. These finishes stay in
Done until focus returns to the pane, including selection in the picker, or new
work starts. Previously acknowledged idle agents remain in date history.
Done is sorted by latest known completion time; agents with no known completion
time follow in the same group.
Idle agents with a recorded completion appear below Active, grouped by local
calendar date and sorted newest first within each date. Idle agents with no
recorded completion and agents with unknown status appear in Other at the end.
The list, statuses, numbers and relative ages refresh every second while the popup
is open. The cursor follows the same agent when the list reorders. Refreshing
stops when the popup closes; temporary API failures preserve the previous list.
Snapshots are fetched in the background so navigation and selection remain
available while Herdr responds. The visible list is replaced only when it changes.

The first nine agents are numbered `1`–`9` in displayed order across date sections;
headings have no number. Press a number to focus immediately. Later agents have no
number; use `j`/`k` or arrow keys and Enter to select them. `0` has no shortcut.

Times persist after a completed agent becomes idle and clear when new work starts.
State is stored under `HERDR_PLUGIN_STATE_DIR/completions`, outside this package.
Standalone runs use `$XDG_STATE_HOME/herdr-agent-picker` (default `~/.local/state`).
Server instances are isolated; completion times from before a server restart are
not reused. The earlier standalone picker's records are read as a migration fallback.

Status colors use the Dracula palette. Model stripping only removes recognized
model metadata separated by ` · `, preserving model names mentioned in task text.

## Validation

```sh
cargo fmt --check
cargo check --locked
cargo test --locked
sh ./run build
sh ./run check
sh ./run test
sh ./run package
sh ./run package --source
```

`run test` builds and tests Rust, then uses Python 3.11+ as a PTY/SQLite fixture
harness around the compiled Rust binary and real fzf. Set `PYTHON` to select the
build/test interpreter; Python is never a plugin runtime dependency. To test a
specific native build directly:

```sh
HERDR_AGENT_PICKER_TEST_BINARY="$PWD/target/release/herdr-agent-picker" \
  python3.11 tests/run_rust_regressions.py
```

Fixtures use synthetic agents, an isolated state directory and fake Herdr
executables. They never change live panes or send desktop notifications. Coverage
includes keyboard/search behavior, live reload latency and cursor identity,
completion migration and hooks, existing reminder schema and server scope,
worker recovery/singleton shutdown, dialogs, ANSI/frame preservation, real-fzf
preview resizing/releasing and forced preview-process death. Notification argument
construction and retry/acknowledgement logic have native unit coverage; actual OS
notification presentation and real Herdr attachment are not automated here.

The former Python runtime and its Python-only unit tests were removed. Neither
`cargo test` nor the explicitly enumerated regression suite can silently fall
back to them. `agent_preview.py` was unused by the baseline picker and is not
part of the port.

Python packaging tooling creates deterministic `.tar.gz`/`.zip` archives and
per-target checksums in `dist/`, using explicit allowlists. Local settings,
sockets, state and caches are excluded. CI tests Rust and real fzf on macOS and
Linux, then builds a native package for each runner and a source archive. A
matching version tag publishes a release only after checks pass (`-preview.*` tags are prereleases); Cargo
and plugin manifest versions must match. Archive reproducibility means identical
archives from identical input files; native compiler output itself is not claimed
to be reproducible across machines. See [CHANGELOG.md](CHANGELOG.md).

Local checks, fixture counts, artifacts and verification limits are recorded in
[the Rust port validation report](docs/rust-port-validation.md).

## Removal

```sh
herdr plugin unlink agent-picker
```

Remove the matching keybinding and reload Herdr config. Unlinking preserves state.

## License

MIT. See LICENSE.
