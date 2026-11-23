# Dialog regression parity

The interactive preview and reminder suites use the native picker, real fzf, and a dedicated PTY and fixture directory for each case. The fake Herdr records argv and uses synthetic identities. Reminder tests initialize the isolated SQLite schema before startup and schedule only future alarms.

## Direct preview input

`tests/interactive.rs` supersedes the removed composer suite. The old assertions
about drafting, text validation and one `agent prompt` submission no longer apply:
input is deliberately delivered immediately, including Enter and control bytes.
All cases use real fzf and a synthetic raw PTY receiver, never a user pane.

Every entered case proves that `x` arrives before any Enter, then decodes actual
painted terminal cells to require both agent rows on the left and `LIVE x` on the
right. Every case keeps the picker open until explicit close/termination, prohibits prompt,
send-text/send-keys, focus and close calls, rejects unexpected commands, checks
all attachment dimensions against fzf's advertised grid and requires attachment
PID death and stopped polling on close. Ordinary closes require status zero;
SIGTERM/SIGKILL require cleanup without another key. Payloads never execute a shell.

| Scenario | Assertions in `tests/interactive.rs` |
| --- | --- |
| `burst` | A single `ix` burst enters input and delivers x before Enter, without a timing guess. |
| `scrolled_entry` | Start scrolled away from the tail; input entry returns to the live end and new output is visible. |
| `malicious_output` | Raw attach and snapshot cursor/OSC/DCS/alternate-screen payloads cannot alter the left list or escape to the outer terminal. |
| `killed_picker`, `terminated_picker` | SIGKILL/SIGTERM to only the picker still reaps the detached attachment and ends polling; SIGTERM never waits for the error prompt. |
| `letters_before_enter` | Letters arrive first, then exactly one raw carriage return. |
| `specials` | Exact arrow, delete/backspace, tab, Ctrl+C/D, NUL and invalid-byte stream. |
| `utf8_fragmented` | Each Korean UTF-8 byte arrives before sending the next. |
| `bracketed_paste` | Exact framing and multiline/control/invalid-byte payload, including an isolated Esc inside paste; no line rejection or prompt call. |
| `delayed` | UTF-8, arrows and deletion still work after 1.5 seconds. |
| `reorder` | Snapshot focus/status/order changes cannot redirect input; j/k, digits, q and Enter go to the original terminal. |
| `escape`, `reenter` | Standalone Esc never reaches the target; re-entry keeps the original attachment. |
| `gone`, `replaced`, `session_replaced`, `terminal_replaced`, `retained_shell`, `lookup_error` | Idle revalidation stops input; subsequent typed bytes and Enter never reach the receiver. |
| `attach_ended`, `read_failure`, `resize` | Stop visibly/closed and return locally via Esc. |
| `search_i`, `empty_list` | Search i stays text; empty rows never start input or look up a foreground process. |
| `origin`, `unknown_origin` | Refuse interactive resize of protected previews. |
| `shell_on_open`, `busy_attach`, `hidden` | No delivery on unsafe, exclusive/busy or unavailable preview paths. |

The picker uses a PTY relay rather than blocking fzf with `execute-silent` for the
whole input session. The short `i` transform acknowledges both normal and search
mode explicitly, so rapid typing is routed after an acknowledged transition.
Private FIFOs keep control separate from arbitrary raw key bytes and bind a
connection to one preview generation. The normal viewport/focus/alarm suites
remain separate regression targets. See [preview parity](parity-preview.md).

## Reminder picker

Every case checks the fictional agent row, then verifies that the preset dialog shows Custom time and Tomorrow 09:00, uses the gray highlight without reverse video, and preserves the picker background without clearing the screen or switching alternate buffers. The dialog emits cursor positions only on rows 4–23 at column 33 in the initial 26 × 130 PTY. Each return to the picker waits for a fresh `Ctrl+D/U` header before sending picker keys. A successful alarm displays its badge. Final picker exit must be successful within five seconds. All cases focus exactly `demo:p1`, except the two custom cancellations, which leave no focus record. Cleanup removes pending or delivering test rows and checks that the detached native worker releases its lock within four seconds when it exists.

| Python scenario | Assertions retained in `tests/reminder_picker.rs` |
| --- | --- |
| `preset_cancel` | Preset 1 stores a five-minute alarm within the original five-second tolerance, shows its badge without focusing, and menu action 9 persists cancellation before picker focus. |
| `custom` | Custom editor shows the local date and time fields. Home/year and tab-separated fields display the year and minute 25. Enter stores exactly the local timestamp two days later at 09:25, displays the badge, and allows focus. |
| `custom_cancel` | Escape from the untouched custom editor leaves no pending row. A second Escape exits the picker without focus. |
| `manager` | Five-minute preset and badge appear without focus. The manager lists scheduled alarms and Enter persists cancellation before picker focus. |
| `escape` | Escape from the preset dialog leaves no pending row, then Enter focuses the picker selection. |
| `custom_escape` | Escape while digits are buffered in the custom editor leaves no pending row. A second Escape exits without focus. |
| `custom_retry` | The editor rejects year 2000 with the future-date message, then accepts the corrected local fields and persists the exact timestamp and badge. |
| `resize` | A live resize to 20 × 100 redraws at row 4, column 18. Down and Up restore preset 1, with the five-minute scheduling, badge, no early focus, and persistent menu cancellation checks. |
| `empty_manager` | The manager reports no scheduled alarms. Escape returns to the picker, followed by the five-minute preset, badge, no early focus, and persistent menu cancellation checks. |
| `jk` | Down, Down, Up via `j`, `j`, `k` highlight 10, 15, and 10 minutes. Enter persists the ten-minute alarm within the original tolerance, with badge, no early focus, and persistent menu cancellation. |
| `delayed_jk` | An ignored `i` stays owned by the alarm dialog; the same navigation, highlighted rows, and scheduling checks pass after a 1.4-second idle. |
| `delayed_arrows` | Down, Down, Up via arrow escape sequences after a 1.4-second idle retain the same highlights and ten-minute scheduling checks. |
| `delayed_custom` | Custom fields still accept Home/year and tab navigation after a 1.4-second idle, with exact timestamp, badge, and focus checks. |
