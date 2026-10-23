"""Small terminal dialogs drawn over the paused picker without clearing it."""

import codecs
import os
import select
import termios
import tty
import unicodedata


# Match fzf's dark palette, with terminal foreground/background for normal rows.
TEXT = '\x1b[0;39;49m'
BORDER = '\x1b[0;38;5;59;49m'
SELECTED = '\x1b[0;39;48;5;236m'


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
            return {b'[A': 'up', b'OA': 'up', b'[B': 'down', b'OB': 'down'}.get(sequence)
        if value in (b'\x03', b'\x04'):
            return 'escape'
        if value in (b'\r', b'\n'):
            return 'enter'
        if value in (b'\x7f', b'\x08'):
            return 'backspace'
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

    def prompt(self, error=''):
        value = ''
        while True:
            self.draw('Custom time', ['Examples: 20m, 1h 30m, 20분 후, 내일 9시',
                                     '2026-12-01 09:00 / tomorrow 09:00',
                                     'Enter: save  Esc / blank: cancel', '',
                                     'When (blank to cancel):', '> ' + value[-45:], error])
            key = self.key()
            if key == 'escape':
                return ''
            if key == 'enter':
                return value.strip()
            if key == 'backspace':
                value = value[:-1]
            elif key and len(key) == 1 and key.isprintable() and len(value) < 200:
                value += key

    def message(self, text):
        while True:
            self.draw('Alarm', [*text.splitlines(), '', 'Enter / Esc: back'])
            if self.key() in ('enter', 'escape'):
                return
