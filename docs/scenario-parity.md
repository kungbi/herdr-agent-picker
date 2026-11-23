# Python harness → native Cargo parity audit

Inventory captured from every `tests/*.py` file before deletion on 2026-11-23.
The 41 picker, 14 live-list, 21 composer, 13 reminder, three conversation-width,
two viewport, four lifecycle and nine origin scenarios retain their individual
Rust test names. Detailed assertion inventories:

| Former file | Native target | Assertion inventory |
| --- | --- | --- |
| `test_picker.py` | `tests/picker.rs` (45 tests) | [Picker inventory](parity-picker.md): all 41 PTY scenarios plus ANSI/newline, Codex/Hermes original snapshots, padded exact text/style checks |
| `test_live_picker.py` | `tests/live_picker.rs` (14 tests) | [Live inventory](parity-picker.md): each reload, selection and latency assertion |
| `test_composer.py` | `tests/composer.rs` (21 tests) | [Composer inventory](parity-dialogs.md): exact argv, pinned identity, editing/paste safety and failure assertions |
| `test_reminder_picker.py` | `tests/reminder_picker.rs` (13 tests) | [Reminder inventory](parity-dialogs.md): each preset/custom/dialog/SQLite/worker assertion |
| `test_conversation_picker.py` | `tests/conversation_picker.rs` (4 tests) | [Conversation inventory](parity-preview.md): padding cases and all three widths |
| `test_preview_resize.py` | `tests/preview_resize.rs` (15 tests) | [Preview inventory](parity-preview.md): current painted cells, scroll, follow, resizing, ownership and cleanup assertions |
| `test_runtime.py` | `tests/runtime.rs` (6 tests) | Exhaustive runtime inventory below |
| `test_package.py` | `tests/package.rs` (2 tests) | Exhaustive package inventory below |
| `rust_support.py` | `tests/common/mod.rs`, `tests/support/fake_herdr.rs` | Fixture and fake inventory below |
| `run_rust_regressions.py` | Cargo integration discovery; `run test` | All eight suites run by `cargo test --locked`, failure propagates; no interpreter runner |

## Runtime assertions

- `native_target_identity`: executable magic must be native ELF/Mach-O; successful
  `--version` output starts with `herdr-agent-picker ` (formerly shared support).
- `hooks_cache_legacy_permissions_and_server_isolation`: independently calculated
  legacy cache filename migrates on focus; pending flag is false and completion
  time remains 42; file/directory modes are 0600/0700. Working clears completion;
  underscore idle event marks pending and completion within five seconds; dotted
  focus clears pending. Dotted closed/exited/agent_detected each remove the pane
  after done. Recreating the socket changes cache path and reminder scope; old
  cache remains done while the new cache records working.
- `cli_and_invalid_preview_dimensions`: shell `open` forwards the exact plugin
  pane argv; native `check` succeeds; native unknown exits 1 and shell unknown
  exits 2. Columns 0, -1, 65536 and invalid produce the unavailable message;
  section rows return successfully; none of these request a pane.
- `killed_preview_releases_attachment_and_lock`: a 43 × 17 preview attaches,
  killing only its process causes the detached guard to release/reap the fake
  within the original deadlines; attachment PID disappears and the exclusive
  nonblocking lock is available. No takeover, focus, write or split resize calls.
- `failed_attachment_spawn_releases_guard_promptly`: remove executable permission
  after pane lookup; preview returns unavailable within two seconds and releases
  the lock without waiting for interactive input.
- `sqlite_worker_recovers_singleton_and_shuts_down_without_notifications`: seed
  the original schema with a future delivering row and stored context; real worker
  recovers it to pending, second worker returns while the first stays alive;
  deleting rows makes the first exit successfully within four seconds; database
  mode is 0600 and fake argv contains no notification.

## Packaging assertions

`deterministic_effective_native_and_source_archives` invokes the real production
`scripts/build_package.py` twice for both native and source packages. **That tool
is the sole Python exception**; no Python test or fixture implementation remains.
Assertions retain equal bytes for tar.gz, zip and checksums, exact independent
allowlists for both formats, executable launcher mode, checksum verification,
valid ZIP CRCs, shell syntax, absent former runtime, native binary mode and an
extracted launcher that runs with invalid Python settings. Added checks compare
every effective TAR/ZIP payload with its input, reject duplicate/nonregular or
unsafe members, and check deterministic timestamps, ownership and ZIP modes.

Extracted Rust source passes `cargo check --locked --offline --all-targets`
in a fresh, separate target directory. This validates the extracted manifest,
lockfile, application and full harness without scanning cached dependency rlibs,
competing for the outer Cargo build lock or executing tests recursively.

`packaging_rejects_bad_tag_and_script_binary` retains mismatched-tag rejection and
rejection of a script masquerading as a native binary.

## Fixtures, fake and execution

`Fixture` independently computes completion SHA-256 identities using socket
inode/ctime nanoseconds and reminder scope using device/inode/ctime; seeds the
same historical done/idle/pending transitions by atomic rename; and provides
SQLite schema/pending/unread/acknowledge/cancel operations through system SQLite.
No fixture imports production state-manipulation code. Commands have isolated
state/config/socket/home paths and remove inherited Herdr/fzf identity settings.

The Rust fake preserves all old response/error/delay/content modes and logs
complete argv as JSON. Preview size, attachment PID and release events are
recorded. It accepts only enumerated synthetic commands, validates prompt/read/
attach argv, and rejects all other commands, including notifications and terminal
input. The fake is an explicitly declared Cargo **example target**, built on
first fixture use with offline, locked Cargo in a separate shared target
directory. Tests read its executable path from Cargo's JSON artifact output,
independent of test executable layout and cached artifact timestamps. Filtered
suites need no preparatory build. Default `cargo install`, release builds and
native distributions contain only the application. Source archives carry the
fake's source so the full harness is reproducible.

The former optional environment selectors (`HERDR_AGENT_PICKER_TEST_CASE` and
`HERDR_COMPOSER_TEST_CASE`) and their unknown-case assertions are replaced by
Cargo's named test filters; all selected-case definitions are enumerated above.
The former binary-exists assertion is supplied by Cargo's executable artifact
binding, with native magic/version still explicitly asserted. SQLite fixture
connection-success assertions are retained in the native FFI fixture.

PTY cases are serialized within each integration target, matching the old
harness's workload and keeping subsecond latency assertions meaningful on CI.
Every wait is bounded; PTY guards kill/reap process groups on failure. Viewport
checks interpret committed screen frames, rather than accepting stale output.
The final-switch origin check waits for a fresh attachment relative to its own
checkpoint, avoiding an old count being satisfied by an intermediate redraw.
