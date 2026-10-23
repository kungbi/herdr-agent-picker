"""Record Herdr completion events without polling or extra picker API calls."""

import time

EVENT_TIME = time.time()

import fcntl
import hashlib
import json
import math
import os
from pathlib import Path


def cache_path():
    socket_path = os.environ.get("HERDR_SOCKET_PATH")
    if not socket_path:
        return None
    try:
        socket_stat = os.stat(socket_path)
    except OSError:
        return None
    # A restarted server must not reuse the previous server's completion times.
    identity = f"{socket_path}:{socket_stat.st_ino}:{socket_stat.st_ctime_ns}"
    key = hashlib.sha256(identity.encode()).hexdigest()[:24]
    state_dir = os.environ.get("HERDR_PLUGIN_STATE_DIR")
    if state_dir:
        root = Path(state_dir)
    else:
        root = Path(os.environ.get("XDG_STATE_HOME", str(Path.home() / ".local/state"))) / "herdr-agent-picker"
    return root / "completions" / f"{key}.json"


def load_times(path):
    if path is None:
        return {}
    try:
        value = json.loads(path.read_text())
        return value if isinstance(value, dict) else {}
    except FileNotFoundError:
        # Preserve times from the earlier standalone picker during migration.
        config = Path(os.environ.get("HERDR_CONFIG_PATH", str(Path.home() / ".config/herdr/config.toml")))
        legacy = config.parent / "agent-completions" / path.name
        try:
            value = json.loads(legacy.read_text())
            return value if isinstance(value, dict) else {}
        except (OSError, ValueError):
            return {}
    except (OSError, ValueError):
        return {}


def record_event(event, path, observed_at):
    if path is None:
        return
    data = event.get("data", {})
    pane_id = data.get("pane_id")
    if not pane_id:
        return
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    with path.with_suffix(".lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        times = load_times(path)
        previous = times.get(pane_id, {})
        if not isinstance(previous, dict):
            previous = {}
        if observed_at < previous.get("observed_at", 0):
            return
        event_name = event.get("event", "").replace(".", "_")
        if event_name == "pane_focused":
            if not previous:
                return
            times[pane_id] = dict(previous, pending_done=False, observed_at=observed_at)
        elif event_name == "pane_agent_status_changed":
            status = data.get("agent_status")
            completed_at = previous.get("completed_at")
            pending_done = pending_completion(previous)
            if status == "done":
                pending_done = False
                if previous.get("status") not in ("done", "idle") or completed_at is None:
                    completed_at = observed_at
            elif status == "idle":
                # Herdr's last-focused pane can finish as idle even while the
                # user is elsewhere. Keep this finish until focus returns.
                if previous.get("status") in ("working", "blocked"):
                    completed_at = observed_at
                    pending_done = True
            else:
                completed_at = None
                pending_done = False
            times[pane_id] = {
                "status": status,
                "completed_at": completed_at,
                "observed_at": observed_at,
                "pending_done": pending_done,
            }
        else:
            # Closed panes and newly detected agents must not inherit old times.
            times.pop(pane_id, None)
        temporary = path.with_suffix(".tmp")
        temporary.write_text(json.dumps(times))
        os.replace(temporary, path)


def pending_completion(entry):
    if "pending_done" in entry:
        return entry["pending_done"] is True
    # Older records timestamped a direct working -> idle finish in both fields.
    # A done -> idle acknowledgement has a later observed_at instead.
    return (entry.get("status") == "idle" and entry.get("completed_at") is not None
            and entry.get("completed_at") == entry.get("observed_at"))


def display_status(agent, times):
    status = agent.get("agent_status")
    entry = times.get(agent["pane_id"], {})
    if (status == "idle" and isinstance(entry, dict)
            and pending_completion(entry) and completion_timestamp(agent, times) is not None):
        return "done"
    return status


def completion_timestamp(agent, times):
    if agent.get("agent_status") not in ("done", "idle"):
        return None
    entry = times.get(agent["pane_id"], {})
    if not isinstance(entry, dict) or entry.get("status") not in ("done", "idle"):
        return None
    completed_at = entry.get("completed_at")
    if not isinstance(completed_at, (int, float)) or not math.isfinite(completed_at):
        return None
    return completed_at


def completion_age(agent, times, now):
    completed_at = completion_timestamp(agent, times)
    if completed_at is None:
        return ""
    elapsed = max(0, int(now - completed_at))
    if elapsed < 10:
        return "just now"
    for unit, seconds in (("d", 86400), ("h", 3600), ("m", 60), ("s", 1)):
        if elapsed >= seconds:
            return f"{elapsed // seconds}{unit} ago"


if __name__ == "__main__":
    record_event(json.loads(os.environ["HERDR_PLUGIN_EVENT_JSON"]), cache_path(), EVENT_TIME)
