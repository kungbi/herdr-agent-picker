# Dialog regression parity

The composer and reminder suites use the native picker, real fzf, and a dedicated PTY and fixture directory for each case. The fake Herdr records argv and uses synthetic identities. Reminder tests initialize the isolated SQLite schema before startup and schedule only future alarms.

## Composer

Every case checks its initial preview or empty-picker message, keeps the picker open before pressing `q`, requires exit status zero, and verifies that no focus or close calls occurred while the picker was open. The final argv log may contain no agent focus, pane send-text, pane send-keys, agent send-keys, or pane close. The shell payload must leave no `NEVER` file. Every opened composer checks the frozen pane, PID, agent name and kind at the bottom row, with exact prompt counts after settling.

| Python case | Assertions retained in `tests/composer.rs` |
| --- | --- |
| `cancel` | Type the literal payload, escape, and verify zero prompts. Open a fresh composer for pane 1 and escape again. |
| `empty` | Enter on empty input keeps the picker alive with zero prompts. Escape returns to the picker. |
| `literal` | A single argv prompt delivers the complete payload, including whitespace, Korean text, quotes, shell substitutions, and metacharacters, to pane 1 after `--`. Reopening targets pane 1. |
| `paste` | Bracketed paste of the same payload submits exactly the literal argv and supports reopening. |
| `paste_multiline` | Bracketed CR/LF paste followed by Enter sends no prompt and keeps the picker alive. |
| `paste_control` | Bracketed escape, ANSI sequence, NUL, tab, and interrupt bytes followed by Enter send no prompt and keep the picker alive. |
| `paste_invalid_utf8` | Bracketed invalid UTF-8 followed by Enter sends no prompt and keeps the picker alive. |
| `unbracketed_multiline` | A burst containing two lines and carriage returns sends no prompt and keeps the picker alive. |
| `utf8_edit` | Fragmented UTF-8, character deletion, and replacement produce exactly one literal prompt with `한끝`. |
| `reorder` | Reverse the snapshot, change focus, status, and title, and wait through fzf's cooked-mode restoration. Preview calls increase and the composer never targets pane 2. Submission and reopening target pane 1, and the new second-agent title appears in the picker. |
| `gone` | Remove pane 1 after opening. Preview calls increase, the recipient never shifts, and Enter reports an error with zero prompts. |
| `replaced` | Change the foreground PID with the same agent kind and session. Preview calls increase, the recipient never shifts, and Enter reports an error with zero prompts. |
| `session_replaced` | Change the session identity. Preview calls increase, the recipient never shifts, and Enter reports an error with zero prompts. |
| `terminal_replaced` | Change the terminal identity. Preview calls increase, the recipient never shifts, and Enter reports an error with zero prompts. |
| `retained_shell` | Move the foreground back to the shell. Preview calls increase, the recipient never shifts, and Enter reports an error with zero prompts. |
| `lookup_error` | Fail the identity lookup. Preview calls increase, the recipient never shifts, and Enter reports an error with zero prompts. |
| `partial_error` | The fake reports partial delivery. The UI shows an error and says the attempt was not retried. Exactly one prompt call reaches the fake. |
| `timeout` | Delay the delivery call beyond its deadline. The UI shows an error and says the attempt was not retried. Exactly one prompt call reaches the fake. |
| `search_i` | Slash enters search, and a query containing `i` is echoed without opening a composer or looking up the pane process. |
| `empty_list` | The empty picker shows “No running agents” and pressing `i` opens no composer. |
| `shell_on_open` | The foreground shell causes “Nothing sent” on open. Escape leaves the picker usable. |

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
| `delayed_jk` | The same navigation, highlighted rows, and scheduling checks pass after a 1.4-second idle. |
| `delayed_arrows` | Down, Down, Up via arrow escape sequences after a 1.4-second idle retain the same highlights and ten-minute scheduling checks. |
| `delayed_custom` | Custom fields still accept Home/year and tab navigation after a 1.4-second idle, with exact timestamp, badge, and focus checks. |
