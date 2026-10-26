# Changelog

## 0.1.0-preview.7

- Stabilize terminal tests by waiting for the picker to redraw after dialogs
  exit, including slow child-process shutdowns.
- Verify completion history correctly when a test run crosses local midnight,
  including the Yesterday-to-date transition.

## 0.1.0-preview.6

- Change Ctrl+D / Ctrl+U to move five agents at a time, regardless of window size.
  Skip section headings and stop at the first or last agent.

## 0.1.0-preview.5

- Replace free-form custom time input with separate numeric date and time fields.
  Use Tab or left/right to select, up/down or j/k to adjust, and digits to replace.
- Prefill the existing reminder or a time 30 minutes ahead, and reject invalid
  calendar dates, past times and nonexistent daylight-saving times.
- Add Ctrl+D / Ctrl+U to move half a screen through agents, skipping headings.

## 0.1.0-preview.4

- Match the picker’s dark selection and border colors in reminder dialogs.
- Keep j/k, arrows and custom typing responsive after fzf pauses its terminal
  input, including when a dialog has been open for more than a second.
- Verify visible selection changes before Enter, plus delayed custom input.

## 0.1.0-preview.3

- Open reminder presets, custom time input and scheduled alarms in small centered
  dialogs over the agent list, preserving the selected agent on return.
- Keep errors and cancellation inside the dialog and support terminal resizing.

## 0.1.0-preview.2

- Press `a` on an agent to set a reminder for 5/10/15/30 minutes, 1/2 hours,
  tomorrow at 09:00, or a custom relative time or date.
- Show scheduled times beside agents; reschedule or cancel with `a`, or manage
  scheduled reminders with `r`.
- Persist reminders locally and deliver them after the picker closes, with
  Herdr and desktop notifications, retrying temporary delivery failures.
- Test relative and absolute times, daylight saving transitions, persistence,
  cancellation, retries, worker recovery and real fzf reminder dialogs.

## 0.1.0-preview.1

First packaged preview, developed on the `preview` branch.

- A Herdr popup with colored agent statuses, task titles and workspace locations.
- Immediate focus with keys 1–9; j/k, arrows and Enter for every agent.
- Live updates to titles, statuses, ordering and completion ages.
- Done and Active above idle history, grouped by local calendar date.
- Track direct working/blocked-to-idle finishes until the pane is visited.
- Preserve cursor identity across updates and keep input responsive during slow snapshots.
- Preserve the list through temporary API failures and stop polling when closed.
- Herdr manifest, Python launcher, event hooks and macOS/Linux release archives.
