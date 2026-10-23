import os
from pathlib import Path
import tempfile
import sys

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from agent_completion_times import cache_path, completion_age, display_status, load_times, record_event


with tempfile.TemporaryDirectory(prefix="herdr-completion-test-") as directory:
    root = Path(directory)
    path = root / "times.json"
    agent = {"pane_id": "w1:p1", "agent_status": "done"}

    def status(value, when, event_name="pane_agent_status_changed"):
        record_event({"event": event_name, "data": {"pane_id": agent["pane_id"], "agent_status": value}}, path, when)

    assert completion_age(agent, load_times(path), 1000) == ""
    status("idle", 100)
    assert completion_age(agent, load_times(path), 110) == ""
    status("working", 200)
    status("done", 300)
    assert completion_age(agent, load_times(path), 305) == "just now"
    assert completion_age(agent, load_times(path), 320) == "20s ago"
    assert completion_age(agent, load_times(path), 480) == "3m ago"
    assert completion_age(agent, load_times(path), 7500) == "2h ago"
    status("done", 350)
    assert load_times(path)["w1:p1"]["completed_at"] == 300
    status("idle", 360)
    agent["agent_status"] = "idle"
    assert completion_age(agent, load_times(path), 480) == "3m ago"
    assert display_status(agent, load_times(path)) == "idle", "Acknowledged done reappeared"
    agent["agent_status"] = "working"
    assert completion_age(agent, load_times(path), 480) == ""
    status("working", 500)
    assert load_times(path)["w1:p1"]["completed_at"] is None
    status("idle", 600, "pane.agent_status_changed")
    agent["agent_status"] = "idle"
    assert completion_age(agent, load_times(path), 660) == "1m ago"
    assert display_status(agent, load_times(path)) == "done", "Direct idle finish was lost"
    status("idle", 610)
    assert display_status(agent, load_times(path)) == "done", "Repeated idle cleared the finish"
    status("working", 550)
    assert completion_age(agent, load_times(path), 660) == "1m ago"
    record_event({'event': 'pane.focused', 'data': {'pane_id': 'w1:p1'}}, path, 620)
    assert display_status(agent, load_times(path)) == "idle", "Focus did not acknowledge completion"
    assert completion_age(agent, load_times(path), 660) == "1m ago"
    status("blocked", 630)
    status("idle", 640)
    assert display_status(agent, load_times(path)) == "done", "Blocked -> idle finish was lost"
    status("working", 650)
    assert display_status(agent, load_times(path)) == "idle", "New work kept the old completion"
    path.write_text('{"w1:p1":{"status":"idle","completed_at":660,"observed_at":660}}')
    assert display_status(agent, load_times(path)) == "done", "Existing direct-idle record was lost"
    record_event({'event': 'pane_focused', 'data': {'pane_id': 'w1:p1'}}, path, 670)
    assert display_status(agent, load_times(path)) == "idle", "Migrated completion reappeared"
    record_event({"event": "pane_agent_detected", "data": {"pane_id": "w1:p1"}}, path, 700)
    assert not load_times(path)
    status("done", 800)
    record_event({"event": "pane_closed", "data": {"pane_id": "w1:p1"}}, path, 900)
    assert not load_times(path)
    path.write_text("broken")
    assert load_times(path) == {}
    status("done", 1000)
    assert load_times(path)["w1:p1"]["completed_at"] == 1000

    os.environ["HERDR_CONFIG_PATH"] = str(root / "config.toml")
    os.environ["HERDR_PLUGIN_STATE_DIR"] = str(root / "plugin-state")
    os.environ.pop("HERDR_SOCKET_PATH", None)
    assert cache_path() is None
    socket = root / "session.sock"
    socket.touch()
    os.environ["HERDR_SOCKET_PATH"] = str(socket)
    first = cache_path()
    legacy = root / 'agent-completions' / first.name
    legacy.parent.mkdir()
    legacy.write_text('{"w1:p1":{"status":"done","completed_at":42,"observed_at":42}}')
    assert load_times(first)['w1:p1']['completed_at'] == 42
    record_event({'event':'pane_agent_status_changed','data':{'pane_id':'w1:p1','agent_status':'idle'}}, first, 100)
    assert first.is_file() and load_times(first)['w1:p1']['completed_at'] == 42
    socket.unlink()
    socket.touch()
    assert cache_path() != first, "Restarted sessions reused cached completion times"
    print("PASS: completion transitions, relative ages, cleanup, legacy migration and server restart isolation")
