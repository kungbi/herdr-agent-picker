from datetime import datetime
import runpy
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

sections = runpy.run_path(str(ROOT / 'agent-picker.py'))['date_sections']
now = datetime(2026, 10, 22, 0, 5).timestamp()
agents = [
    {'pane_id': 'blocked', 'agent_status': 'blocked'},
    {'pane_id': 'working', 'agent_status': 'working'},
    {'pane_id': 'today_old', 'agent_status': 'done'},
    {'pane_id': 'yesterday', 'agent_status': 'idle'},
    {'pane_id': 'old', 'agent_status': 'done'},
    {'pane_id': 'today_new', 'agent_status': 'idle'},
    {'pane_id': 'undated', 'agent_status': 'done'},
    {'pane_id': 'unknown', 'agent_status': 'unknown'},
]
times = {}
for pane_id, completed_at in [
    ('today_old', datetime(2026, 10, 22, 0, 1)),
    ('today_new', datetime(2026, 10, 22, 0, 4)),
    ('yesterday', datetime(2026, 10, 21, 23, 59)),
    ('old', datetime(2026, 10, 19, 12, 0)),
]:
    times[pane_id] = {'status': 'done', 'completed_at': completed_at.timestamp()}
result = [(title, [agent['pane_id'] for agent in items]) for title, items in sections(agents, times, now)]
assert result == [
    ('Done', ['today_old', 'old', 'undated']),
    ('Active', ['blocked', 'working']),
    ('Today', ['today_new']),
    ('Yesterday', ['yesterday']),
    ('Other', ['unknown']),
], result
assert sections([], {}, now) == []
assert [title for title, _ in sections(agents[:2], {}, now)] == ['Active']
assert [title for title, _ in sections(agents[-2:], {}, now)] == ['Done', 'Other']
assert sections([{'pane_id': 'idle', 'agent_status': 'idle'}], {}, now) == [
    ('Other', [{'pane_id': 'idle', 'agent_status': 'idle'}]),
]
assert [title for title, _ in sections([agents[3]], times, now)] == ['Yesterday']
times['today_new'].update(status='idle', pending_done=True)
result = sections(agents, times, now)
assert [agent['pane_id'] for agent in result[0][1]] == ['today_new', 'today_old', 'old', 'undated']
assert result[0][1][0]['agent_status'] == 'done'
assert agents[5]['agent_status'] == 'idle', 'Rendering mutated the API snapshot'
times['today_new']['pending_done'] = False
assert [title for title, _ in sections([agents[5]], times, now)] == ['Today']
print('PASS: current Done/Active above idle history, newest Done first, dated idle history, no empty groups')
