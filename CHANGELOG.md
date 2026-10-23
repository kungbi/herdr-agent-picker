# Changelog

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
