"""Small terminal dialogs drawn over the paused picker without clearing it."""

import codecs
import os
import select
import termios
import tty
import unicodedata


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
        tty.setraw(self.fd)
        self.decoder = codecs.getincrementaldecoder('utf-8')('replace')
        self.write('\x1b[?25l')
        return self

    def __exit__(self, *_):
        termios.tcsetattr(self.fd, termios.TCSANOW, self.attributes)
        self.write('\x1b[0m\x1b[?25h')
        os.close(self.fd)

    def write(self, text):
        os.write(self.fd, text.encode())

    def key(self):
        if not select.select([self.fd], [], [], 0.1)[0]:
            return None  # Polling also lets a resize redraw the dialog.
        value = os.read(self.fd, 1)
        if not value:
            return 'escape'
        if value == b'\x1b':
            sequence = b''
            while select.select([self.fd], [], [], 0.04)[0]:
                sequence += os.read(self.fd, 1)
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
        output = ['\x1b[0m\x1b[?25l']
        for index, line in enumerate(panel):
            color = '\x1b[7m' if index - 1 == selected else '\x1b[0m'
            output.append(f'\x1b[{top + index};{left}H{color}{line}\x1b[0m')
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
            self.draw(title, headings + [''] + [' ' + label for label in visible],
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
