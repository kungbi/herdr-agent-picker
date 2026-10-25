"""Small terminal dialogs drawn over the paused picker without clearing it."""

import codecs
import calendar
from datetime import datetime, timedelta
import os
import select
import termios
import tty
import time
import unicodedata


# Match fzf's dark palette, with terminal foreground/background for normal rows.
TEXT = '\x1b[0;39;49m'
BORDER = '\x1b[0;38;5;59;49m'
SELECTED = '\x1b[0;39;48;5;236m'


class DateTimeForm:
    """Numeric date/time fields; incomplete edits stay visible until committed."""

    def __init__(self, initial=None):
        value = datetime.fromtimestamp(initial) if initial is not None else datetime.now() + timedelta(minutes=30)
        self.values = [value.year, value.month, value.day, value.hour, value.minute]
        self.index, self.draft = 2, None

    def commit(self):
        if self.draft:
            value = int(self.draft)
            low, high = ((1, 9999), (1, 12), (1, 31), (0, 23), (0, 59))[self.index]
            if not low <= value <= high:
                name = ('Year', 'Month', 'Day', 'Hour', 'Minute')[self.index]
                raise ValueError(f'{name}: use {low}-{high}.')
            self.values[self.index] = value
        self.draft = None

    def move(self, step):
        self.commit()
        self.index = (self.index + step) % 5

    def digit(self, value):
        width = 4 if self.index == 0 else 2
        self.draft = (self.draft or '') + value if len(self.draft or '') < width else value

    def backspace(self):
        self.draft = (str(self.values[self.index]) if self.draft is None else self.draft)[:-1]

    def parts(self):
        parts = [f'{self.values[0]:04d}', *(f'{value:02d}' for value in self.values[1:])]
        parts[self.index] = f'[{self.draft if self.draft is not None else parts[self.index]}]'
        return parts

    def adjust(self, step):
        self.commit()
        year, month, day, hour, minute = self.values
        if self.index in (0, 1):
            if self.index == 0:
                year += step
            else:
                offset = year * 12 + month - 1 + step
                year, month = offset // 12, offset % 12 + 1
            if not 1 <= year <= 9999:
                raise ValueError('Year: use 1-9999.')
            day = min(day, calendar.monthrange(year, month)[1])
            value = datetime(year, month, day, hour, minute)
        else:
            value = datetime(*self.values) + timedelta(**{('days', 'hours', 'minutes')[self.index - 2]: step})
        self.values = [value.year, value.month, value.day, value.hour, value.minute]

    def due(self, now=None):
        self.commit()
        value = datetime(*self.values)
        timestamp = value.timestamp()
        if datetime.fromtimestamp(timestamp) != value:
            raise ValueError('This local time does not exist. Choose another time.')
        if timestamp <= (time.time() if now is None else now):
            raise ValueError('Choose a future date and time.')
        return timestamp


def fitted(text, width):
    result, used = '', 0
    for character in str(text):
        if not character.isprintable():
            continue
        cells = 0 if unicodedata.combining(character) else 2 if unicodedata.east_asian_width(character) in 'WF' else 1
        if used + cells > width:
            break
        result += character
        used += cells
    return result + ' ' * max(0, width - used)


class Dialog:
    def __enter__(self):
        self.fd = os.open('/dev/tty', os.O_RDWR)
        self.attributes = termios.tcgetattr(self.fd)
        tty.setraw(self.fd, termios.TCSANOW)
        os.set_blocking(self.fd, False)
        self.decoder = codecs.getincrementaldecoder('utf-8')('replace')
        self.write('\x1b[?25l')
        return self

    def __exit__(self, *_):
        termios.tcsetattr(self.fd, termios.TCSANOW, self.attributes)
        self.write('\x1b[0m\x1b[?25h')
        os.close(self.fd)

    def write(self, text):
        remaining = memoryview(text.encode())
        while remaining:
            try:
                remaining = remaining[os.write(self.fd, remaining):]
            except BlockingIOError:
                select.select([], [self.fd], [], 0.1)

    def key(self):
        # fzf execute-silent restores cooked input after one second. Keep this
        # dialog in raw mode so navigation never starts waiting for Enter.
        if termios.tcgetattr(self.fd)[3] & (termios.ICANON | termios.ECHO | termios.ISIG):
            tty.setraw(self.fd, termios.TCSANOW)
        if not select.select([self.fd], [], [], 0.1)[0]:
            return None  # Polling also lets a resize redraw the dialog.
        try:
            value = os.read(self.fd, 1)
        except BlockingIOError:
            # A mode change between select and read must not block the UI.
            return None
        if not value:
            return 'escape'
        if value == b'\x1b':
            sequence = b''
            while select.select([self.fd], [], [], 0.04)[0]:
                try:
                    chunk = os.read(self.fd, 1)
                except BlockingIOError:
                    break
                if not chunk:
                    return 'escape'
                sequence += chunk
                if len(sequence) >= 2 and 0x40 <= sequence[-1] <= 0x7e:
                    break
            if not sequence:
                return 'escape'
            return {b'[A': 'up', b'OA': 'up', b'[B': 'down', b'OB': 'down',
                    b'[C': 'right', b'OC': 'right', b'[D': 'left', b'OD': 'left',
                    b'[Z': 'shift-tab', b'[H': 'home', b'OH': 'home', b'[1~': 'home',
                    b'[F': 'end', b'OF': 'end', b'[4~': 'end'}.get(sequence)
        if value in (b'\x03', b'\x04'):
            return 'escape'
        if value in (b'\r', b'\n'):
            return 'enter'
        if value in (b'\x7f', b'\x08'):
            return 'backspace'
        if value == b'\t':
            return 'tab'
        return self.decoder.decode(value) or None

    def draw(self, title, lines, selected=None):
        columns, rows = os.get_terminal_size(self.fd)
        width = min(66, max(4, columns - 4))
        # Keep every step in the same rectangle, so a shorter custom-input
        # screen also covers all of the preceding preset menu.
        height = min(14, max(3, rows - 2))
        top, left = max(1, (rows - height) // 2 + 1), max(1, (columns - width) // 2 + 1)
        inner = width - 2
        border_title = fitted(' ' + title + ' ', max(0, inner - 2)).rstrip()
        panel = ['╭' + ('─' + border_title).ljust(inner, '─')[:inner] + '╮']
        content = (lines + [''] * height)[:height - 2]
        panel += ['│' + fitted(line, inner) + '│' for line in content]
        panel += ['╰' + '─' * inner + '╯']
        output = [TEXT + '\x1b[?25l']
        for index, line in enumerate(panel):
            if index in (0, len(panel) - 1):
                styled = BORDER + line
            else:
                color = SELECTED if index - 1 == selected else TEXT
                styled = BORDER + line[0] + color + line[1:-1] + BORDER + line[-1]
            output.append(f'\x1b[{top + index};{left}H{styled}\x1b[0m')
        self.write(''.join(output))

    def choose(self, rows, header, title='Alarm'):
        selected = 0
        labels = [row.partition('\t')[2] for row in rows]
        while True:
            _, terminal_rows = os.get_terminal_size(self.fd)
            headings = header.splitlines()[:2]
            capacity = max(1, min(9, terminal_rows - len(headings) - 7))
            start = max(0, selected - capacity + 1)
            visible = labels[start:start + capacity]
            self.draw(title, headings + [''] + [
                ('> ' if start + index == selected else '  ') + label
                for index, label in enumerate(visible)
            ],
                      len(headings) + 1 + selected - start)
            key = self.key()
            if key == 'escape':
                return None
            if key == 'enter':
                return rows[selected].partition('\t')[0]
            if key in ('j', 'down'):
                selected = min(len(rows) - 1, selected + 1)
            elif key in ('k', 'up'):
                selected = max(0, selected - 1)
            elif key and key in '123456789' and int(key) <= len(rows):
                return rows[int(key) - 1].partition('\t')[0]

    def date_time(self, initial=None):
        form, error = DateTimeForm(initial), ''
        while True:
            year, month, day, hour, minute = form.parts()
            self.draw('Custom time', [
                'Date and time (local)',
                '←/→ / Tab: field   ↑/↓ / j/k: change', '',
                f'Date:  {year} - {month} - {day}',
                f'Time:  {hour} : {minute}', '',
                'Digits: replace selected field',
                'Enter: save   Esc: cancel', '', error,
            ], 3 if form.index < 3 else 4)
            key = self.key()
            if key == 'escape':
                return None
            try:
                if key == 'enter':
                    return form.due()
                if key in ('left', 'h', 'shift-tab'):
                    form.move(-1)
                elif key in ('right', 'l', 'tab'):
                    form.move(1)
                elif key in ('home', 'end'):
                    form.commit()
                    form.index = 0 if key == 'home' else 4
                elif key in ('up', 'k', 'down', 'j'):
                    form.adjust(1 if key in ('up', 'k') else -1)
                elif key == 'backspace':
                    form.backspace()
                elif key and key in '0123456789':
                    form.digit(key)
                if key is not None:
                    error = ''
            except (ValueError, OverflowError) as problem:
                error = str(problem)

    def message(self, text):
        while True:
            self.draw('Alarm', [*text.splitlines(), '', 'Enter / Esc: back'])
            if self.key() in ('enter', 'escape'):
                return
