# Picker regression coverage

The Cargo integration targets `picker` and `live_picker` run the native binary against real fzf in a PTY with isolated fake Herdr data.

| Python fixture | Rust coverage |
| --- | --- |
| `test_picker.py` direct preview | `preview_preserves_ansi_and_newline`, `codex_reads_original_terminal_snapshot`, `hermes_reads_original_terminal_snapshot`, `padded_preview_preserves_styles_and_indentation` |
| `test_picker.py` alarm and quit | `alarm_unread_cancel`, `alarm_unread_open`, `quit` |
| `test_picker.py` search | `search_q_text`, `search_escape_quit`, `search_title`, `search_workspace`, `search_agent_name`, `search_escape_empty`, `search_escape_text`, `search_escape_number`, `search_escape_no_match`, `search_escape_navigation`, `search_escape_preview`, `search_escape_reenter` |
| `test_picker.py` preview | `preview_padded`, `preview_default`, `preview_move`, `preview_toggle`, `preview_hidden`, `preview_long_line` |
| `test_picker.py` movement | `select`, `cancel`, `cross_up`, `arrows`, `navigate_tenth`, `navigate_last`, `jump_down`, `jump_up`, `jump_small_down`, `jump_small_up`, `jump_up_top`, `jump_down_end`, `jump_up_end` |
| `test_picker.py` shortcuts | `number_first`, `number_date_section`, `number_last`, `number_missing`, `number_first_large_list`, `number_ninth`, `number_zero_ignored` |
| `test_live_picker.py` refreshed rows and selection | `preserve_cursor`, `new_agent_shortcut`, `changed_section_shortcut`, `done_only`, `third_done_via_idle`, `removed_agent`, `initially_empty`, `becomes_empty` |
| `test_live_picker.py` refresh failures and recovery | `failed_refresh`, `refresh_recovers` |
| `test_live_picker.py` delayed snapshots | `slow_navigation`, `slow_enter`, `slow_number`, `slow_preserve_cursor` |

The 41 interactive picker scenarios check initial preview, exact Herdr preview arguments, snapshot calls, and focus before and after selection. Smaller lists also check completion age, dated history, section order, status colors, hidden model metadata, and visible shortcut numbers. Search cases check focus and the absence of the mode marker during partial input. Alarm cases check the red marker and retain unread state on cancel. Preview cases check wrapping, colors, padding, indentation, blank rows, literal pipes, and suppressed reads while hidden.

The 14 live scenarios check data and shortcut changes, cursor preservation, completion acknowledgement, empty-list behavior, polling shutdown, and invalid-snapshot recovery. The three immediate input cases require focus within 700 ms of sending the key during a 1.5-second refresh, and require the picker to exit within two seconds after focus.

Build the fake Herdr target once, then run a single scenario with Cargo's test filter.

```sh
cargo test --test fake_herdr --no-run
cargo test --test picker search_escape_preview
cargo test --test live_picker slow_navigation
```
