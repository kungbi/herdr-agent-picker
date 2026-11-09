"""ANSI-preserving terminal cleanup and small, private per-popup view state."""
import fcntl
import json
import os
import re
import unicodedata

STYLES = re.compile(r'\x1b\[[0-9;:]*m')
MAX_OFFSET = 65536
DEFAULT_STATE = {'offset': 0, 'wrap': False, 'full': False, 'paused': False}


def cell_width(character):
    if unicodedata.category(character) in ('Mn', 'Me', 'Cf'):
        return 0
    return 2 if unicodedata.east_asian_width(character) in ('W', 'F') else 1


def clean_snapshot(text):
    rows = [(line, STYLES.sub('', line).rstrip('\r\n'))
            for line in text.splitlines(keepends=True)]
    # Only a blank framed row establishes an outer edge, not a literal table.
    frame_columns = {len(plain) for _, plain in rows if re.fullmatch(r' +[│┃]', plain)}
    cleaned = []
    for line, plain in rows:
        if plain.endswith(('│', '┃')) and sum(map(cell_width, plain)) in frame_columns:
            line = re.sub(r'([│┃])((?:\x1b\[[0-9;:]*m)*)$', r'\2', line, flags=re.MULTILINE)
        cleaned.append(line)
    text = re.sub(r'(?:[ \t]|\x1b\[[0-9;:]*m)+$',
                  lambda match: ''.join(STYLES.findall(match[0])), ''.join(cleaned), flags=re.MULTILINE)
    # Ignore blank rows; remove only ASCII spaces common to every content row.
    plain_rows = [plain for line in text.splitlines()
                  if (plain := STYLES.sub('', line)).strip()]
    # Moving a tab to a different stop can change relative code indentation.
    if any(plain.lstrip(' ').startswith('\t') for plain in plain_rows):
        return text
    indents = [len(plain) - len(plain.lstrip(' ')) for plain in plain_rows]
    gutter = min(indents, default=0)
    if not gutter:
        return text
    result = []
    for line in text.splitlines(keepends=True):
        remaining = gutter
        parts = []
        for token in re.split(r'(\x1b\[[0-9;:]*m)', line):
            if STYLES.fullmatch(token):
                parts.append(token)
            else:
                count = min(remaining, len(token) - len(token.lstrip(' ')))
                parts.append(token[count:])
                remaining -= count
        result.append(''.join(parts))
    return ''.join(result)


def read_view(path):
    state = DEFAULT_STATE.copy()
    if path is None:
        return state
    try:
        with path.open() as source:
            value = json.loads(source.read(256))
        offset = value.get('offset', 0)
        if type(offset) is int:
            state['offset'] = max(0, min(MAX_OFFSET, offset))
        for key in ('wrap', 'full', 'paused'):
            state[key] = value.get(key) is True
    except (OSError, ValueError, TypeError, AttributeError):
        pass
    return state


def save_view(path, state):
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(state, separators=(',', ':')))
    os.replace(temporary, path)


def view_action(path, action):
    with path.with_suffix('.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        state = read_view(path)
        if action == 'pause':
            state['paused'] = True
        elif action in ('live', 'resume'):
            state['paused'] = False
        elif action in ('wrap', 'full'):
            state[action] = not state[action]
            state['paused'] = False
            if action == 'wrap':
                state['offset'] = 0
        elif action in ('left', 'right'):
            state['offset'] = max(0, min(MAX_OFFSET, state['offset'] + (8 if action == 'right' else -8)))
            state['wrap'] = False
            state['paused'] = False
        else:
            raise ValueError('Unknown preview control')
        save_view(path, state)
    if action == 'pause':
        return 'change-preview-label( Paused · Shift+End: live )'
    if action in ('live', 'resume'):
        return 'change-preview-label( Preview )' + ('+preview-bottom+refresh-preview' if action == 'resume' else '')
    # fzf caps side previews at 99%; a lower panel is genuinely full-width
    # while retaining a few list rows and the original Enter target above it.
    layout = 'down:80%' if state['full'] else 'right:60%'
    wrapping = 'wrap-word' if state['wrap'] else 'nowrap'
    return f'change-preview-label( Preview )+change-preview-window({layout}:{wrapping})+refresh-preview'


def pan_snapshot(text, offset, columns, state_path=None):
    # Expand tabs by terminal cells; never slice an ANSI sequence or half a CJK glyph.
    rows = []
    longest = 0
    for line in text.splitlines(keepends=True):
        tokens = []
        cells = 0
        for token in re.split(r'(\x1b\[[0-9;:]*m)', line):
            if STYLES.fullmatch(token):
                tokens.append((token, 0, True))
                continue
            for character in token:
                width = 8 - cells % 8 if character == '\t' else cell_width(character)
                if character in '\r\n':
                    tokens.append((character, 0, True))
                else:
                    tokens.append((' ' * width if character == '\t' else character, width, False))
                    cells += width
        longest = max(longest, cells)
        rows.append(tokens)
    requested_offset = offset
    offset = min(offset, max(0, longest - max(1, columns) + 1))
    if state_path is not None and offset != requested_offset:
        with state_path.with_suffix('.lock').open('a') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            current = read_view(state_path)
            # A cancelled preview must not overwrite a newer key press.
            if current['offset'] == requested_offset and not current['wrap']:
                current['offset'] = offset
                save_view(state_path, current)
    output = []
    for tokens in rows:
        cells = 0
        visible = False
        for token, width, control in tokens:
            if control:
                output.append(token)
            elif cells >= offset and (width or visible):
                output.append(token)
                visible = True
            elif cells < offset < cells + width:
                output.append(' ' * (cells + width - offset))
                visible = False
            cells += width
    return ''.join(output)
