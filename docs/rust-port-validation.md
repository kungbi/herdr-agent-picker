# Rust port validation

Validated on 2026-11-23 in the existing `feat/rust-port` worktree, based on
`0569c4816bf812af3d5a0b4c03bd9299a158d9ef`. No commit, push, merge, installation,
or live Herdr pane operation was performed.

Environment: macOS 26.5.2, arm64; rustc/cargo 1.94.1; fzf 0.73.1;
Python 3.14.4 for test/package tooling. All Cargo resolution/builds used cached
crates offline. There are five direct dependencies: libc, regex, serde_json,
sha2 and unicode-width. SQLite uses the system library with owned statements,
parameter binding, explicit transactions and a three-second busy timeout.

## Runtime delivered

- Rust implements the picker, search/keyboard bindings, background fzf reloads,
  ANSI terminal previews, completion event hooks, reminders, detached worker and
  overlaid terminal dialogs. Runtime callbacks invoke this same native executable.
- Completion SHA-256 identities, legacy cache fallback, event spellings, pending
  idle completion acknowledgement, ordering and local date groups are retained.
- The original SQLite tables/columns, scope identity, stored delivery contexts,
  replacement/cancellation, unread state, metadata tokens and retries are retained.
  Reminder management still lists pending reminders across scopes.
- Preview attachments use the measured fzf grid and never request takeover,
  terminal input, split resizing or focus. A detached Rust guard holds the preview
  lock until attachment teardown/reaping. Pipe EOF also releases an attachment
  when the preview process is forcibly killed.
- The launcher uses a packaged executable or the checkout's release build. Hooks
  and popups never compile or invoke Python. Python runtime files and Python-only
  runtime unit tests were removed; the unused saved-conversation renderer was
  not part of the baseline picker's active path.
- Packaging emits target-specific native archives and a separate source archive.
  CI is wired to native Rust checks, real fzf and packaging on macOS/Linux.

## Exact checks

These commands passed in the checkout:

```sh
cargo fmt --check
cargo check --offline --locked
cargo test --offline --locked
cargo clippy --offline --locked --all-targets -- -D warnings
sh ./run build --offline
HERDR_AGENT_PICKER_TEST_BINARY="$PWD/target/release/herdr-agent-picker" \
  CARGO_NET_OFFLINE=true sh ./run test
sh ./run package --tag v0.1.0-preview.7
sh ./run package --source --tag v0.1.0-preview.7
(cd dist && shasum -a 256 -c SHA256SUMS-aarch64-apple-darwin)
(cd dist && shasum -a 256 -c SHA256SUMS-source)
sh -n run
git diff --check
```

Native unit tests: **13 passed**, including a separate process with the
America/New_York timezone for DST/calendar checks. Coverage includes completion
transitions and stale events, dates/ordering, TOML sort-setting forms, title
metadata, ANSI cleanup, reminder replacement/scopes/retries/acknowledgement,
notification argument construction and sidebar context/token commands.

The explicit Python harness executes **75 real-fzf PTY scenarios against the
release Rust executable**:

| Suite | PTY scenarios | Evidence |
| --- | ---: | --- |
| Picker | 41 | Search/Escape, numbers, headings, jumps, ANSI, focus/unread semantics |
| Live list | 14 | Cursor identity, empty/removed agents, failed/recovered reloads, input during 1.5s API delay |
| Native preview | 3 | 72/100/160 columns, Codex/Hermes parity, wrapping, periodic updates, hiding |
| Preview lifecycle | 4 | Switch/hide/show/close, resize, interrupt, attach failure, read failure |
| Reminder dialogs | 13 | Presets, custom dates, cancellation, manager, resize, delayed raw-mode input and worker shutdown |

Additional native CLI fixture checks cover hook/cache migration and permissions,
server restart isolation, `open`/`check`/unknown commands, invalid preview sizes,
forced preview SIGKILL cleanup, failed attachment spawn, and real worker recovery,
singleton ownership and exit. No fixture sends a desktop notification.

Packaging tests compare repeated archive bytes and exact allowlisted contents,
check modes/checksums/tag rejection, reject scripts posing as native binaries,
launch the extracted executable with invalid Python settings, and run offline
`cargo check` on extracted Rust source. Mach-O inspection confirms the release
binary links system SQLite/System/iconv, with no Python runtime.

The active regression helper rejects script executables and prints the native
binary path under test. It imports no former runtime modules. Historical event
and SQLite helpers only seed synthetic state; Rust assertions and subprocesses
provide runtime coverage. Test temporary directories and final logs are under
`target/test-tmp` and `target/test-results` in this checkout.

Two inherited harness timing issues were fixed: output draining is bounded even
while fzf animates, and exiting after the final key has its own deadline instead
of reusing the popup-opening deadline. A fresh completion-age fixture is seeded
for each popup so slow runs cannot invalidate a hardcoded three-minute age.

## Artifacts and remaining verification limits

`dist/` contains `.tar.gz` and `.zip` packages for `aarch64-apple-darwin` and for
`source`, with corresponding `SHA256SUMS-*` files. The native package contains no
Python runtime or Python dependency. Source test/build tools need Python 3.11+.
Archive reproducibility is verified for identical input files; this does not
claim bit-for-bit compiler reproducibility across hosts.

- Linux and other CPU architectures were not built/executed locally: only the
  aarch64 macOS Rust target is installed here. Linux CI is configured, but was
  not triggered. A Linux binary is not included in the local `dist/` output.
- Real Herdr server attachment, desktop notification presentation and sound were
  not exercised. Tests use fake Herdr executables and synthetic terminal/state
  fixtures, as required to avoid real panes and notifications.
- The small config reader extracts `ui.agent_panel_sort` from table, dotted,
  quoted/escaped-key and inline-table forms. It does not perform general TOML
  validation of unrelated settings; malformed config may be tolerated where
  Python's former `tomllib` reader would reject the entire file.
- Source preview limitations remain: complex joined emoji widths and terminal
  content without a blank outer-frame reference are not reconstructed.
