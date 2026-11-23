# Native Rust validation

The entire former `tests/*.py` integration/regression harness and its fake Herdr
are now native Rust integration suites and a fixture example used by
`cargo test --locked`.
The [scenario parity audit](scenario-parity.md) was captured before removing the
Python files and links each original scenario and assertion to its replacement.
The former composer is superseded by direct PTY input; see the updated
[interactive preview inventory](parity-dialogs.md).

## Running the checks

Requirements: Rust, fzf 0.73.1, system SQLite development libraries, `gzip`,
`unzip`, and Python 3.11+ for the real production packaging tool only. On Debian/
Ubuntu, the system packages are `libsqlite3-dev`, `gzip` and `unzip`; macOS ships
the SQLite library and archive utilities. Set `PYTHON` if packaging needs a
specific interpreter. No Python tests, fixtures or embedded fixture scripts exist.
`scripts/build_package.py` is the sole Python exception: Rust packaging tests
invoke that production tool and independently inspect its results.

With locked dependencies already cached:

```sh
export CARGO_NET_OFFLINE=true
export CARGO_TARGET_DIR="$(mktemp -d)"
cargo fmt --check
cargo check --locked
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
sh -n run
git diff --check
```

`sh ./run test` forwards directly to `cargo test --locked`, including additional
Cargo arguments. Cargo supplies the application binary path to tests, with no
prebuilt release requirement. All eight integration suites run by default;
none is ignored. Filter a suite directly from a clean checkout; its Rust fake
builds automatically on first use:

```sh
cargo test --locked --test interactive bracketed_paste
cargo test --locked --test live_picker slow_navigation
```

The `fake_herdr` fixture is an explicitly declared `[[example]]`, not a `[[bin]]`.
Each integration executable builds it once with offline, locked Cargo, using a
shared, separate target directory beside the Cargo-supplied application path.
Cargo handles cache freshness and reports the actual executable in its documented
[JSON artifact output](https://doc.rust-lang.org/cargo/reference/external-tools.html#artifact-messages).
Tests never scan executable siblings or choose artifacts by timestamp. This works
with filtered suites, relocated test executables and custom `CARGO_TARGET_DIR`
values. Default `cargo install` and release builds exclude examples; native
archives contain only the application. Source archives include the fake's Rust
source and the full harness.

## Behavior and isolation

The native harness covers 41 picker PTY scenarios, 14 mutable-list scenarios,
29 direct interactive preview scenarios, 13 reminder dialogs, three conversation widths, two
viewport cases, four attachment lifecycle cases and nine origin-capture cases.
Additional direct checks cover preview formatting, fixture compatibility,
CLI/hooks, native process death and lock cleanup, SQLite worker recovery and
singleton ownership, packaging rejection and deterministic archives.

Each case gets fresh isolated socket/config/state/home paths. The fixture removes
inherited Herdr/fzf settings and fake control flags before applying its own
environment. The native fake records argv, rejects unexpected commands and
never invokes real Herdr or notification services. Reminder fixtures schedule
only future alarms and explicitly cancel rows and wait for worker lock release.
SQLite fixtures use independent system-library calls and the original schema.

Cases serialize within each integration executable, including fixture setup, so
completion ages are fresh and slow-snapshot input latency assertions measure the
application rather than competing terminal tests. The 700 ms input and two-second
exit deadlines during a 1.5-second API delay are retained. Process waits, output
draining and cleanup are bounded. Failed PTY cases kill/reap their process group;
attachment guard tests also require detached process death and lock availability.
Failed fixture directories are retained under `target/test-tmp` for diagnosis.

Preview scrolling checks decode completed terminal frames and examine the actual
preview cells across idle/growing/rolling/replaced snapshots. Conversation tests
retain ANSI sequences, Korean and combining text, literal tables, indentation,
blank rows and all three widths. Interactive preview checks pin pane/PID/session/terminal identity, prove raw byte
delivery before Enter, preserve paste/control/UTF-8 bytes, decode left/right
painted cells and require visible closed failures and attachment cleanup.

## Package verification

```sh
sh ./run package --binary "${CARGO_TARGET_DIR:-target}/release/herdr-agent-picker"
sh ./run package --source
(cd dist && shasum -a 256 -c SHA256SUMS-aarch64-apple-darwin)
(cd dist && shasum -a 256 -c SHA256SUMS-source)
```

Use the checksum filename for the actual host target. Rust tests compare both
native and source tar.gz/zip outputs byte for byte across two builds, inspect
independent exact member allowlists, every effective payload, timestamps, modes,
ownership, checksums and ZIP CRCs. The extracted native launcher runs with Python
settings pointing to nonexistent paths. Extracted source runs
`cargo check --locked --offline --all-targets` in its own fresh target directory.
This checks the extracted manifest, lockfile, application, fixture example and
integration suites without reusing outer build artifacts or executing tests
recursively. Both nested Cargo commands use separate target directories to avoid
contending for the outer Cargo build lock.

Archive reproducibility means identical archives for identical inputs. It does
not claim identical compiler outputs across hosts. Native archives contain only
the production executable, launcher, manifests and documentation. Source archives
contain the native tests and production packaging tool, with no built helper,
local caches, sockets, state, settings or target directories.

## Validation environment and limits

The direct-input and preview-height changes passed these local checks with cached dependencies:

```sh
cargo fmt --check
cargo check --locked --offline
cargo clippy --locked --offline --all-targets -- -D warnings
cargo test --locked --offline
cargo build --release --locked --offline
cargo test --release --locked --offline --test interactive letters_before_enter -- --exact
sh -n run
git diff --check
sh ./run --version
```

The default suite passed **17 unit tests and 129 integration tests**, with none
ignored, including
29 direct-input cases, 45 picker cases, 14 live-list cases, 16 preview cases,
13 reminder cases, four conversation checks, six runtime checks and two package
checks. The package test checks native/source determinism, exact effective
contents, checksums, extracted launcher execution and extracted-source compilation.
The release binary is rebuilt in `target/release/herdr-agent-picker`, which this
linked checkout's `run` selects (there is no packaged `bin/` override). The
additional release-mode `letters_before_enter` case passed against that executable.
`sh ./run --version` reports `herdr-agent-picker 0.2.0`.

Changed files for this revision:

- Application: `src/interaction.rs` (new), `src/picker.rs`, `src/preview.rs`,
  `src/dialog.rs`, `src/main.rs`; removed `src/composer.rs`.
- Regression fixtures: `tests/interactive.rs` (new), `tests/common/mod.rs`,
  `tests/support/fake_herdr.rs`, `tests/preview_resize.rs`, `tests/reminder_picker.rs`,
  `tests/package.rs`;
  removed `tests/composer.rs`. The terminal-screen decoder was moved into shared
  test support so interactive and viewport tests use the same painted-cell model.
- Packaging/launcher: `scripts/build_package.py`, `run`.
- Documentation: `README.md`, `CHANGELOG.md`, `docs/parity-dialogs.md`,
  `docs/parity-preview.md`, `docs/scenario-parity.md`, `docs/rust-port-validation.md`.

Local validation uses macOS arm64, rustc/cargo 1.94.1, fzf 0.73.1 and cached dependencies offline.
CI runs the same default Cargo suite, formatting, check, clippy and release build
on macOS and Linux. Dependency/fzf provisioning occurs before offline validation
in CI. Linux CI is configured; it has not been triggered from this checkout.
No commit, push, merge, tag, release or global installation is part of validation.

PR3 revalidation used a fresh temporary `CARGO_TARGET_DIR` for the full offline
suite and build checks. A second empty target directory passed a filtered picker
test without a preparatory helper build. Picker and package test executables also
passed from separate `debug/build/herdr-agent-picker/<test>/out` directories with
newer decoy fake executables and invalid dependency rlibs beside them, while the
runtime `CARGO_TARGET_DIR` pointed elsewhere. These checks exercise relocation
and stale-artifact handling without changing coverage or lint levels.

Real Herdr attachment and desktop notification presentation/sound remain outside
these synthetic tests. A disposable Herdr 0.9.5 server was attempted with isolated
home/config/socket paths; this sandbox rejected startup with `Operation not
permitted`, so no live user server or pane was contacted. The existing preview limitations for complex joined emoji
widths and terminal content without a blank outer-frame reference are unchanged.
The small config reader extracts `ui.agent_panel_sort` and does not generally
validate unrelated TOML settings.
