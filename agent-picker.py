#!/usr/bin/env python3
"""Pick an agent by number, or navigate and press Enter to focus."""

import json
from datetime import datetime
import fcntl
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib

from agent_completion_times import (
    cache_path, completion_age, completion_timestamp, display_status, load_times, record_event,
)
from agent_reminders import alarm_badges, ensure_worker


HERDR = os.environ.get("HERDR_BIN_PATH", "herdr")
FZF = shutil.which("fzf")


def request(*arguments):
    completed = subprocess.run(
        [HERDR, *arguments], text=True, capture_output=True, check=True, timeout=3
    )
    response = json.loads(completed.stdout)
    if "error" in response:
        raise RuntimeError(response["error"].get("message", "Herdr request failed"))
    return response["result"]


def label(value):
    # Keep each item on one line and prevent terminal controls in agent titles.
    return " ".join("".join(c if c.isprintable() else " " for c in str(value or "")).split())


def display_title(value):
    # Remove model metadata separated by middle dots, preserving task text.
    parts = label(value).split(" · ")
    model = r"(?:[\w.-]+/)?(?:gpt-[\w.-]+|claude-[\w.-]+|gemini-[\w.-]+|o\d[\w.-]*)"
    return " · ".join(
        part for index, part in enumerate(parts)
        if index == 0 or not re.fullmatch(model, part, re.IGNORECASE)
    )


def status_label(status):
    # Match Herdr's Dracula status palette and dot indicators.
    colors = {
        "working": (241, 250, 140),
        "blocked": (255, 85, 85),
        "done": (139, 233, 253),
        "idle": (80, 250, 123),
        "unknown": (98, 114, 164),
    }
    red, green, blue = colors.get(status, colors["unknown"])
    icon = "○" if status == "idle" else "·" if status == "unknown" else "●"
    return f"\033[38;2;{red};{green};{blue}m{icon} [{label(status)}]\033[39m"


def date_sections(agents, times, now):
    done, active, other, dates = [], [], [], {}
    today = datetime.fromtimestamp(now).date()
    for agent in agents:
        agent = dict(agent, agent_status=display_status(agent, times))
        if agent["agent_status"] == "done":
            done.append(agent)
            continue
        if agent["agent_status"] in ("blocked", "working"):
            active.append(agent)
            continue
        completed_at = completion_timestamp(agent, times)
        if completed_at is None:
            other.append(agent)
        else:
            day = datetime.fromtimestamp(completed_at).date()
            dates.setdefault(day, []).append((completed_at, agent))
    sections = []
    if done:
        done.sort(key=lambda agent: (
            completion_timestamp(agent, times) is not None,
            completion_timestamp(agent, times) or 0,
        ), reverse=True)
        sections.append(("Done", done))
    if active:
        sections.append(("Active", active))
    for day in sorted(dates, reverse=True):
        days_ago = (today - day).days
        heading = "Today" if days_ago == 0 else "Yesterday" if days_ago == 1 else day.isoformat()
        items = sorted(dates[day], key=lambda item: item[0], reverse=True)
        sections.append((heading, [agent for _, agent in items]))
    if other:
        sections.append(("Other", other))
    return sections


def render_rows():
    snapshot = request("api", "snapshot")["snapshot"]
    agents = [agent for agent in snapshot["agents"] if agent.get("agent")]
    if not agents:
        return ["@section\t── No running agents ──"], 1

    workspaces = {item["workspace_id"]: item["label"] for item in snapshot["workspaces"]}
    tabs = {item["tab_id"]: item["label"] for item in snapshot["tabs"]}
    config_path = Path(os.environ.get("HERDR_CONFIG_PATH", str(Path.home() / ".config/herdr/config.toml")))
    try:
        with config_path.open("rb") as config_file:
            config = tomllib.load(config_file)
    except FileNotFoundError:
        config = {}
    if config.get("ui", {}).get("agent_panel_sort") == "priority":
        priority = {"blocked": 4, "working": 3, "done": 2, "idle": 1, "unknown": 0}
        agents.sort(key=lambda agent: (priority.get(agent["agent_status"], 0), agent.get("state_change_seq", 0)), reverse=True)

    rows = []
    number = 0
    times = load_times(cache_path())
    alarms = alarm_badges()
    now = time.time()
    selected_index = 2
    for heading, items in date_sections(agents, times, now):
        if heading in ("Active", "Done", "Other"):
            color = "139;233;253" if heading == "Done" else "189;147;249"
            heading_label = f"\033[1;38;2;{color}m── {heading} ──\033[22;39m"
        else:
            heading_label = f"\033[2m── {heading} ──\033[22m"
        rows.append(f"@section\t{heading_label}")
        for agent in items:
            number_label = "    "
            number += 1
            if number <= 9:
                number_label = f"[{number}] "
            if number == 1 or agent.get("focused"):
                selected_index = len(rows) + 1
            location = "/".join(label(value) for value in (
                workspaces.get(agent["workspace_id"], agent["workspace_id"]),
                tabs.get(agent["tab_id"], agent["tab_id"]),
            ))
            name = label(agent.get("name") or agent.get("display_agent") or agent["agent"])
            title = display_title(agent.get("terminal_title_stripped") or agent.get("title")) or name
            age = completion_age(agent, times, now)
            age_label = f"\033[2m{age}\033[22m " if age else ""
            alarm_label = ''
            if agent['pane_id'] in alarms:
                due = datetime.fromtimestamp(alarms[agent['pane_id']])
                alarm_label = f" \033[38;2;255;184;108m[alarm {due:%m-%d %H:%M}]\033[39m"
            rows.append(
                f"{agent['pane_id']}\t{number_label}{status_label(agent['agent_status'])} "
                f"{age_label}"
                f"\033[1m{title}\033[22m "
                f"\033[2m· {location}\033[22m{alarm_label}"
            )
    return rows, selected_index


def refresh_rows(path):
    try:
        rows, _ = render_rows()
    except (OSError, ValueError, KeyError, RuntimeError, subprocess.SubprocessError):
        # A temporary API failure must not blank an already usable list.
        return path.read_text()
    content = "\n".join(rows) + "\n"
    temporary = path.with_suffix(".tmp")
    temporary.write_text(content)
    os.replace(temporary, path)
    return content


def refresh_action(path):
    # Only fetching the ready local file may use fzf's blocking ID tracking.
    # API calls run in a background transform, leaving the visible list usable.
    with path.with_suffix(".lock").open("a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return ""
        previous = path.read_text()
        if refresh_rows(path) == previous:
            return ""
        return f"reload-sync:cat {shlex.quote(str(path))}"


def main():
    if FZF is None:
        raise RuntimeError("fzf is required; see README.md for installation instructions")
    rows, selected_index = render_rows()
    try:
        ensure_worker()
    except (OSError, RuntimeError):
        # Scheduling errors are shown in the alarm menu; keep agent navigation usable.
        pass

    environment = os.environ.copy()
    for key in ("FZF_DEFAULT_OPTS", "FZF_DEFAULT_OPTS_FILE"):
        environment.pop(key, None)
    number_bindings = []
    for number in range(1, 10):
        # Count matches rather than raw rows, so headings and reloads cannot
        # make a shortcut point at a different agent than its displayed number.
        actions = "pos(1)" + "+down-match" * number + "+accept"
        number_bindings.append(
            f'--bind={number}:transform:if [ "$FZF_MATCH_COUNT" -ge {number} ]; '
            f"then printf '%s' '{actions}'; fi"
        )
    # Count agents rather than section headings, independently of window size.
    jump_bindings = [
        f"--bind={key}:" + "+".join([f"{direction}-match"] * 5)
        for key, direction in (("ctrl-d", "down"), ("ctrl-u", "up"))
    ]
    with tempfile.TemporaryDirectory(prefix="herdr-agent-picker-") as directory:
        cache = Path(directory) / "rows"
        content = "\n".join(rows) + "\n"
        cache.write_text(content)
        reload_command = shlex.join([
            sys.executable, str(Path(__file__).resolve()), "--refresh", str(cache),
        ])
        reminder_command = shlex.join([sys.executable, str(Path(__file__).with_name('agent_reminders.py').resolve())])
        choice = subprocess.run(
            [FZF, "--ansi", "--color=dark,fg+:-1", "--sync", "--no-input", "--no-sort", "--no-multi", "--layout=reverse",
             "--track", "--id-nth=1",
             # Raw mode displays non-matching section headings; match navigation skips them.
             "--raw", "--query=!^──", "--with-shell=/bin/sh -c",
             "--delimiter=\t", "--with-nth=2..", "--border=rounded", "--border-label= Agents ",
             "--header=1-9/Enter: focus  j/k ↑↓: move  Ctrl+D/U: 5 agents  a: alarm  r: alarms  Esc: close",
             f"--bind=j:down-match,k:up-match,down:down-match,up:up-match,ctrl-j:down-match,ctrl-k:up-match,esc:abort,load:pos({selected_index})+unbind(load)",
             "--bind=enter:transform:if [ -n {1} ] && [ {1} != '@section' ]; then printf accept; fi",
             "--bind=double-click:transform:if [ -n {1} ] && [ {1} != '@section' ]; then printf accept; fi",
             f"--bind=every(1):bg-transform:{reload_command}",
             f"--bind=a:execute-silent:{reminder_command} menu {{1}}",
             f"--bind=r:execute-silent:{reminder_command} list",
             "--bind=a:+clear-screen,r:+clear-screen",
             *number_bindings, *jump_bindings],
            input=content, text=True, stdout=subprocess.PIPE, env=environment,
        )
    if choice.returncode in (1, 130):
        return 0
    if choice.returncode:
        raise RuntimeError(f"fzf exited with status {choice.returncode}")
    pane_id, separator, _ = choice.stdout.partition("\t")
    if not separator or not pane_id or pane_id == "@section":
        raise RuntimeError("Invalid agent selection")
    request("agent", "focus", pane_id)
    record_event({"event": "pane.focused", "data": {"pane_id": pane_id}}, cache_path(), time.time())
    return 0


if __name__ == "__main__":
    try:
        if len(sys.argv) == 3 and sys.argv[1] == "--rows":
            # Keep already-open popups compatible until they are reopened.
            sys.stdout.write(refresh_rows(Path(sys.argv[2])))
        elif len(sys.argv) == 3 and sys.argv[1] == "--refresh":
            sys.stdout.write(refresh_action(Path(sys.argv[2])))
        else:
            sys.exit(main())
    except (OSError, ValueError, KeyError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"Agent picker error: {error}", file=sys.stderr)
        if sys.stdin.isatty():
            input("Press Enter to close")
        sys.exit(1)
