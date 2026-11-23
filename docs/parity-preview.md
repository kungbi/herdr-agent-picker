# Native preview regression parity

The Rust integration tests use a synthetic Herdr process and a real fzf controlling terminal. The tests check that terminal attaches carry only the pane identifier and preview reads use `--source recent-unwrapped --lines N --format ansi`, where N is the preview height or 30, whichever is larger. The fake records the attachment PID, its PTY size, and the size fzf advertised.

## Preview viewport and lifecycle

| Python scenario | Rust case and preserved assertions |
| --- | --- |
| Viewport with origin `p1` | The picker starts at the bottom with row 30 visible and row 1 absent. Five Shift-Up commands reveal row 12 and hide row 30. The same painted cells remain after three idle reads, three reads of grown output, and two rolling-window reads. Shift-Down returns to row 34 and the next snapshot follows to row 35. A nonoverlapping replacement shows row 30 with the edit suffix and every visible row is edited. The origin never attaches. |
| Viewport with origin `outside` | The same painted-cell and polling checks apply when the selected pane is attached. Exactly one attachment occurs and its PID disappears after closing. |
| Switch, hide, and close | The first attach matches fzf's preview dimensions and precedes its pane read. Switching to p2 reaps p1 and logs its release before the p2 attachment. Resizing from 100 to 72 columns replaces the p2 process with the updated correct dimensions. Hiding reaps the resized process and stops reads for 1.3 seconds. Showing attaches p2 a third time and abort reaps that process. |
| Interrupt | The initial sizing and read-order checks apply. Ctrl-C reaps the attachment. |
| Read failure | The initial sizing and read-order checks apply. A failed pane read reaps the attachment. |
| Attach failure | fzf paints the unavailable message and no pane read occurs. |

The viewport assertions inspect committed terminal frames and restrict row matching to columns 43–96, so old PTY output cannot satisfy the scroll checks.

## Picker origin capture

| Python scenario | Rust case and preserved assertions |
| --- | --- |
| `session-origin` | Session focus resolves p2 even when both rows advertise focus. |
| `origin-not-selected` | The first selection attaches to p1. Moving to p2 reaps it and uses native reads. Subsequent p1 attachment counts account for the initial attachment. |
| `plugin-origin` | Plugin context resolves p2 even though session focus reports the popup. |
| `context-only` | Plugin context resolves p2 when session focus is absent. |
| `missing-origin` | Missing focus and context produce read-only previews for both rows. |
| `conflicting-origin` | Conflicting p1 context and p2 session focus produce read-only previews for both rows. |
| `stale-context` | Stale context with p2 session focus produces read-only previews for both rows. |
| `popup-only` | Popup focus without context produces read-only previews for both rows. |
| `origin-read-failure` | Failed native p2 read paints the unavailable message without get or attach fallback. |

All nine scenarios prohibit p2 get/attach and popup attach, and prohibit all get/attach before native polling except for the initial p1 attachment in `origin-not-selected`. Successful p2 previews refresh without status changes and render `SCREEN_p2`. Resize and show trigger additional reads, with no get or attach while native preview is active. Hiding allows an in-flight read to settle and then stops reads for 1.3 seconds. Switching to p1 keeps read-only behavior when the origin is unresolved. With a resolved origin, p1 attaches at matching dimensions and remains the other pane when a refreshed snapshot reports its focus and changed title. Switching back to p2 reaps the p1 attachment, orders release before the latest p2 read, and continues to refresh p2 without attachment. The changed label is painted before issuing the next navigation key, avoiding a backend-read/UI-reload race. Returning to p1 again attaches, and closing reaps it. Every scenario closes successfully, emits no unexpected fake commands, and stops polling after close.

## Native conversation rendering

| Python scenario | Rust case and preserved assertions |
| --- | --- |
| Frame padding | Padding lengths 0, 1, 2, 8, 31, and 32 preserve the exact unstyled Korean, combining mark, indentation, markdown literal, table edge, and blank line. Every source ANSI style sequence survives in order. |
| Literal table and ANSI padding | A literal table with a 40-space span and a literal heavy vertical edge survives unchanged. Interleaved ANSI padding is stripped from the text while all style sequences survive in order. |
| Direct Hermes and Codex previews at 72, 100, and 160 columns | Both direct previews exit successfully, preserve the exact native unstyled content and source ANSI sequence order, and keep the source line count. |
| Real picker at 72, 100, and 160 columns | fzf renders the wrap terminator, first response, literal code markup, Korean text, and original truecolor sequence. Saved-conversation and assistant transcript labels are absent. The picker closes successfully without changing focus and calls only snapshot, read, get, and attach. |
| Real picker updates at 100 columns | Changed content appears without moving the selection. Moving down shows the second response. Hiding stops reads after a settling period, and showing picks up content changed while hidden. The picker remains alive after resizing to 72 columns. |

## Direct input extension

The existing read-only/origin, viewport, attachment lifecycle and conversation
checks remain. `tests/interactive.rs` adds direct raw input through the same
preview attachment while a separate fzf PTY continues rendering. Input mode pins
list reloads and revalidates terminal/session/foreground identity before delivery.
Standalone Esc is local; unsafe/busy/unavailable/protected previews fail visibly
without fallback. See the [interactive inventory](parity-dialogs.md). Snapshot
sanitization removes cursor/alternate-screen/OSC/DCS commands while retaining
SGR. Attachment stream escapes never reach the outer terminal.
