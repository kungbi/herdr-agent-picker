//! Extract one setting without pulling a general TOML dependency into hooks.
//! Strings/comments and nested values are tokenized so unrelated config content
//! cannot be mistaken for a UI setting. The user's config is never rewritten.
#[derive(Debug)]
enum Token {
    Text(String, bool),
    Mark(char),
    Newline,
}
fn quoted(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, quote: char) -> Option<String> {
    let mut look = chars.clone();
    let multiline = look.next() == Some(quote) && look.next() == Some(quote);
    if multiline {
        chars.next();
        chars.next();
        if chars.peek() == Some(&'\r') {
            chars.next();
        }
        if chars.peek() == Some(&'\n') {
            chars.next();
        }
    }
    let mut value = String::new();
    while let Some(c) = chars.next() {
        if c == quote {
            if !multiline {
                return Some(value);
            }
            let mut look = chars.clone();
            if look.next() == Some(quote) && look.next() == Some(quote) {
                chars.next();
                chars.next();
                // TOML permits one or two quotes immediately before closing.
                while chars.peek() == Some(&quote) {
                    value.push(chars.next()?);
                }
                return Some(value);
            }
        }
        if c == '\\' && quote == '"' {
            let escape = chars.next()?;
            match escape {
                'b' => value.push('\x08'),
                't' => value.push('\t'),
                'n' => value.push('\n'),
                'f' => value.push('\x0c'),
                'r' => value.push('\r'),
                '"' => value.push('"'),
                '\\' => value.push('\\'),
                'u' | 'U' => {
                    let mut n = 0;
                    for _ in 0..if escape == 'u' { 4 } else { 8 } {
                        n = n * 16 + chars.next()?.to_digit(16)?;
                    }
                    value.push(char::from_u32(n)?);
                }
                c if multiline && c.is_whitespace() => {
                    while chars.peek().is_some_and(|c| c.is_whitespace()) {
                        chars.next();
                    }
                }
                _ => return None,
            }
        } else {
            value.push(c);
        }
    }
    None
}
fn tokens(text: &str) -> Option<Vec<Token>> {
    let mut chars = text.chars().peekable();
    let mut tokens = Vec::new();
    while let Some(c) = chars.next() {
        match c {
            '\n' => tokens.push(Token::Newline),
            c if c.is_whitespace() => (),
            '#' => {
                while chars.peek().is_some_and(|c| *c != '\n') {
                    chars.next();
                }
            }
            '"' | '\'' => tokens.push(Token::Text(quoted(&mut chars, c)?, true)),
            '[' | ']' | '{' | '}' | '.' | ',' | '=' => tokens.push(Token::Mark(c)),
            c => {
                let mut word = c.to_string();
                while chars
                    .peek()
                    .is_some_and(|c| !c.is_whitespace() && !"[]{}.,=#\"'".contains(*c))
                {
                    word.push(chars.next()?);
                }
                tokens.push(Token::Text(word, false));
            }
        }
    }
    Some(tokens)
}
struct Reader {
    tokens: Vec<Token>,
    index: usize,
    priority: bool,
}
impl Reader {
    fn mark(&mut self, c: char) -> bool {
        if matches!(self.tokens.get(self.index),Some(Token::Mark(m)) if *m==c) {
            self.index += 1;
            true
        } else {
            false
        }
    }
    fn whitespace(&mut self) {
        while matches!(self.tokens.get(self.index), Some(Token::Newline)) {
            self.index += 1;
        }
    }
    fn path(&mut self) -> Option<Vec<String>> {
        let mut path = Vec::new();
        loop {
            let Some(Token::Text(text, _)) = self.tokens.get(self.index) else {
                return None;
            };
            path.push(text.clone());
            self.index += 1;
            if !self.mark('.') {
                return Some(path);
            }
        }
    }
    fn value(&mut self, path: &[String]) {
        if self.mark('{') {
            loop {
                self.whitespace();
                if self.mark('}') || self.index >= self.tokens.len() {
                    break;
                }
                let before = self.index;
                if let Some(mut key) = self.path() {
                    if self.mark('=') {
                        let mut nested = path.to_vec();
                        nested.append(&mut key);
                        self.value(&nested);
                    }
                }
                if self.index == before {
                    self.index += 1;
                }
                self.mark(',');
            }
        } else if self.mark('[') {
            let mut depth = 1;
            while self.index < self.tokens.len() && depth > 0 {
                if self.mark('[') {
                    depth += 1;
                } else if self.mark(']') {
                    depth -= 1;
                } else {
                    self.index += 1;
                }
            }
        } else {
            if path == ["ui", "agent_panel_sort"] {
                self.priority = matches!(self.tokens.get(self.index),Some(Token::Text(s,true)) if s=="priority");
            }
            while self.index < self.tokens.len()
                && !matches!(
                    self.tokens[self.index],
                    Token::Newline | Token::Mark(',' | '}')
                )
            {
                self.index += 1;
            }
        }
    }
}
pub fn priority(text: &str) -> bool {
    let Some(tokens) = tokens(text) else {
        return false;
    };
    let mut reader = Reader {
        tokens,
        index: 0,
        priority: false,
    };
    let mut section = Vec::new();
    while reader.index < reader.tokens.len() {
        reader.whitespace();
        if reader.mark('[') {
            let array = reader.mark('[');
            section = reader.path().unwrap_or_default();
            reader.mark(']');
            if array {
                reader.mark(']');
                section.push("[]".into());
            }
        } else if let Some(mut key) = reader.path() {
            if reader.mark('=') {
                let mut path = section.clone();
                path.append(&mut key);
                reader.value(&path);
            }
        }
        while reader.index < reader.tokens.len()
            && !matches!(reader.tokens[reader.index], Token::Newline)
        {
            reader.index += 1;
        }
    }
    reader.priority
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn toml_sort_forms_and_unrelated_content() {
        for text in [
            "[ui]\nagent_panel_sort = \"priority\" # comment",
            "ui.agent_panel_sort = 'priority'",
            "'ui' . \"agent_panel_sort\" = \"priority\"",
            "ui = { other = [1, 2], agent_panel_sort = 'priority' }",
            "[\"ui\"]\nagent_panel_sort = \"\\u0070riority\"",
            "[ui]\nagent_panel_sort = \"\"\"\npriority\"\"\"",
            "text = '''\n[ui]\nagent_panel_sort = 'other'\n'''\n[ui]\nagent_panel_sort='priority'",
        ] {
            assert!(priority(text), "{text}");
        }
        for text in [
            "",
            "[other]\nagent_panel_sort='priority'",
            "# [ui]\nagent_panel_sort='priority'",
            "[ui]\nagent_panel_sort='activity'",
            "items=[{ui={agent_panel_sort='priority'}}]",
            "text=\"[ui] agent_panel_sort='priority'\"",
        ] {
            assert!(!priority(text), "{text}");
        }
    }
}
