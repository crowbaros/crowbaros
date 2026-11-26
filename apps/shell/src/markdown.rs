//! Reads an answer as the Markdown a Model writes: paragraphs, headings,
//! lists, quotes, fenced code, tables, and, within a line, strong and
//! emphasised words, struck words, inline code, and links.
//!
//! It reads what Models write, and is no whole CommonMark: what it does not
//! know stays as it was typed. An answer is read again as each piece of it
//! arrives, so a mark that has not been closed yet, as a fence still being
//! written, reads as what it will be.

use std::ops::Range;

/// A part of an answer that stands apart from the parts beside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Paragraph(Line),
    Heading { level: u8, line: Line },
    /// Fenced code, as it was written, and the language its fence named.
    Code { language: Option<String>, text: String },
    List(Vec<Item>),
    Quote(Line),
    /// A table's rows, each as it was written.
    Table(Vec<String>),
    Rule,
}

/// One item of a list: how deep it is, what marks it, and what it says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub depth: usize,
    pub marker: String,
    pub line: Line,
}

/// Text with its marks taken out, and how each part of it is to look.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    /// The parts that are not plain, in order, none over another.
    pub spans: Vec<Span>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    pub look: Look,
}

/// How a part of a line looks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Look {
    pub strong: bool,
    pub emphasis: bool,
    pub strike: bool,
    pub code: bool,
    /// Where a link goes.
    pub link: Option<String>,
}

impl Look {
    fn plain(&self) -> bool {
        *self == Look::default()
    }
}

impl Line {
    fn push(&mut self, text: &str, look: &Look) {
        if text.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(text);
        if look.plain() {
            return;
        }
        match self.spans.last_mut() {
            Some(last) if last.range.end == start && last.look == *look => last.range.end = self.text.len(),
            _ => self.spans.push(Span {
                range: start..self.text.len(),
                look: look.clone(),
            }),
        }
    }
}

/// Reads one line's worth of words: `**strong**`, `*emphasis*`, `~~struck~~`,
/// `` `code` ``, `[a link](where)`, and a bare `https://` address.
pub fn line(source: &str) -> Line {
    let mut out = Line::default();
    inline(source, &Look::default(), &mut out);
    out
}

fn inline(source: &str, look: &Look, out: &mut Line) {
    let bytes = source.as_bytes();
    let mut at = 0;
    let mut plain_from = 0;
    macro_rules! flush {
        ($upto:expr) => {
            out.push(&source[plain_from..$upto], look);
        };
    }
    while at < bytes.len() {
        let rest = &source[at..];
        let before = source[..at].chars().next_back();
        match bytes[at] {
            b'\\' if rest[1..].chars().next().is_some_and(|c| c.is_ascii_punctuation()) => {
                flush!(at);
                plain_from = at + 1;
                at += 2;
            }
            b'`' => {
                let ticks = rest.bytes().take_while(|b| *b == b'`').count();
                let fence = &rest[..ticks];
                // Code closes on as many ticks as opened it, and holds
                // whatever is between as it is.
                let close = rest[ticks..].match_indices(fence).map(|(i, _)| i).find(|i| {
                    let after = ticks + i + ticks;
                    !rest[after..].starts_with('`') && !rest[..ticks + i].ends_with('`')
                });
                match close {
                    Some(close) => {
                        flush!(at);
                        let mut code = &rest[ticks..ticks + close];
                        if code.len() > 2 && code.starts_with(' ') && code.ends_with(' ') {
                            code = &code[1..code.len() - 1];
                        }
                        let mut inner = look.clone();
                        inner.code = true;
                        out.push(code, &inner);
                        at += ticks + close + ticks;
                        plain_from = at;
                    }
                    None => at += ticks,
                }
            }
            b'*' | b'_' | b'~' => {
                let mark = bytes[at];
                let run = rest.bytes().take_while(|b| *b == mark).count();
                let width = match (mark, run) {
                    (b'~', 2..) => 2,
                    (b'~', _) => 0,
                    (_, 3..) => 3,
                    (_, 2) => 2,
                    _ => 1,
                };
                // An underscore inside a word, as in `a_name`, is a letter.
                let in_word = mark == b'_' && before.is_some_and(char::is_alphanumeric);
                let opens = width > 0 && !in_word && rest[width..].chars().next().is_some_and(|c| !c.is_whitespace());
                let close = opens
                    .then(|| closing(&rest[width..], &rest[..width], mark == b'_'))
                    .flatten();
                match close {
                    Some(close) => {
                        flush!(at);
                        let mut inner = look.clone();
                        match (mark, width) {
                            (b'~', _) => inner.strike = true,
                            (_, 3) => (inner.strong, inner.emphasis) = (true, true),
                            (_, 2) => inner.strong = true,
                            _ => inner.emphasis = true,
                        }
                        inline(&rest[width..width + close], &inner, out);
                        at += width + close + width;
                        plain_from = at;
                    }
                    None => at += run.max(1),
                }
            }
            b'[' => {
                let link = rest.find("](").and_then(|mid| {
                    let label = &rest[1..mid];
                    let end = rest[mid + 2..].find(')')?;
                    let target = rest[mid + 2..mid + 2 + end].trim();
                    (!label.is_empty() && !label.contains('\n') && !target.is_empty() && !target.contains(' '))
                        .then_some((label, target, mid + 2 + end + 1))
                });
                match link {
                    Some((label, target, len)) => {
                        flush!(at);
                        let mut inner = look.clone();
                        inner.link = Some(target.to_string());
                        inline(label, &inner, out);
                        at += len;
                        plain_from = at;
                    }
                    None => at += 1,
                }
            }
            b'h' if (rest.starts_with("https://") || rest.starts_with("http://"))
                && !before.is_some_and(char::is_alphanumeric)
                && look.link.is_none() =>
            {
                let end = rest.find(|c: char| c.is_whitespace() || c == '<' || c == '>').unwrap_or(rest.len());
                // What ends a sentence is not part of the address.
                let address = rest[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '\'', '"']);
                if address.len() > "https://".len() {
                    flush!(at);
                    let mut inner = look.clone();
                    inner.link = Some(address.to_string());
                    out.push(address, &inner);
                    at += address.len();
                    plain_from = at;
                } else {
                    at += 1;
                }
            }
            _ => at += rest.chars().next().map_or(1, char::len_utf8),
        }
    }
    flush!(bytes.len());
}

/// Where `mark` closes what it opened, in the text after the opening: at a
/// mark with words before it, and, for an underscore, no word after it.
fn closing(after: &str, mark: &str, whole_words: bool) -> Option<usize> {
    let mut from = 0;
    while let Some(found) = after[from..].find(mark) {
        let at = from + found;
        let before = after[..at].chars().next_back();
        let next = after[at + mark.len()..].chars().next();
        let escaped = before == Some('\\');
        let closes = at > 0
            && before.is_some_and(|c| !c.is_whitespace())
            && !escaped
            // `**` is not the end of a single `*`.
            && !(mark.len() == 1 && (next.is_some_and(|c| mark.starts_with(c)) || after[..at].ends_with(mark)))
            && !(whole_words && next.is_some_and(char::is_alphanumeric));
        if closes {
            return Some(at);
        }
        from = at + mark.len();
    }
    None
}

/// The fence a line opens or closes: its mark, and what follows it.
fn fence(line: &str) -> Option<(&str, &str)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let trimmed = &line[indent..];
    let mark = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let run = trimmed.chars().take_while(|c| *c == mark).count();
    (indent < 4 && run >= 3).then(|| (&trimmed[..run], trimmed[run..].trim()))
}

/// A list item's line: how far in it is, its marker, and what it says.
fn list_item(line: &str) -> Option<(usize, String, &str)> {
    let indent = line.len() - line.trim_start().len();
    let trimmed = &line[indent..];
    let (marker, rest) = if let Some(rest) = ["- ", "* ", "+ "].iter().find_map(|m| trimmed.strip_prefix(m)) {
        ("•".to_string(), rest)
    } else {
        let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
        let after = &trimmed[digits..];
        if digits == 0 || digits > 9 || !(after.starts_with(". ") || after.starts_with(") ")) {
            return None;
        }
        (format!("{}.", &trimmed[..digits]), &after[2..])
    };
    Some((indent / 2, marker, rest.trim()))
}

fn rule(line: &str) -> bool {
    let marks: Vec<char> = line.chars().filter(|c| !c.is_whitespace()).collect();
    marks.len() >= 3 && matches!(marks[0], '-' | '*' | '_') && marks.iter().all(|c| *c == marks[0])
}

fn heading(line: &str) -> Option<(u8, &str)> {
    let level = line.bytes().take_while(|b| *b == b'#').count();
    let rest = &line[level..];
    ((1..=6).contains(&level) && (rest.is_empty() || rest.starts_with(' ')))
        .then(|| (level as u8, rest.trim().trim_end_matches('#').trim_end()))
}

/// Reads an answer into its blocks.
pub fn blocks(source: &str) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let mut paragraph: Vec<&str> = Vec::new();
    let mut lines = source.lines().peekable();

    fn end_paragraph(paragraph: &mut Vec<&str>, out: &mut Vec<Block>) {
        if !paragraph.is_empty() {
            out.push(Block::Paragraph(line(&paragraph.join(" "))));
            paragraph.clear();
        }
    }

    while let Some(raw) = lines.next() {
        let trimmed = raw.trim();
        if let Some((mark, info)) = fence(raw) {
            end_paragraph(&mut paragraph, &mut out);
            let mut text = String::new();
            for inner in lines.by_ref() {
                // A fence closes on a line of its own mark, as long or longer.
                if fence(inner).is_some_and(|(close, rest)| close.starts_with(mark) && rest.is_empty()) {
                    break;
                }
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(inner);
            }
            let language = info.split_whitespace().next().map(str::to_string);
            out.push(Block::Code { language, text });
        } else if trimmed.is_empty() {
            end_paragraph(&mut paragraph, &mut out);
        } else if let Some((level, text)) = heading(trimmed) {
            end_paragraph(&mut paragraph, &mut out);
            out.push(Block::Heading { level, line: line(text) });
        } else if rule(trimmed) && paragraph.is_empty() {
            out.push(Block::Rule);
        } else if let Some((depth, marker, text)) = list_item(raw) {
            end_paragraph(&mut paragraph, &mut out);
            let mut said = text.to_string();
            // What follows an item, further in and with no marker of its
            // own, is more of the item.
            while let Some(next) = lines.peek() {
                if next.trim().is_empty() || !next.starts_with(' ') || list_item(next).is_some() || fence(next).is_some() {
                    break;
                }
                said.push(' ');
                said.push_str(next.trim());
                lines.next();
            }
            let item = Item { depth, marker, line: line(&said) };
            match out.last_mut() {
                Some(Block::List(items)) => items.push(item),
                _ => out.push(Block::List(vec![item])),
            }
        } else if let Some(quoted) = trimmed.strip_prefix('>') {
            end_paragraph(&mut paragraph, &mut out);
            let mut said = quoted.trim().to_string();
            while let Some(next) = lines.peek().and_then(|l| l.trim().strip_prefix('>')) {
                said.push(' ');
                said.push_str(next.trim());
                lines.next();
            }
            out.push(Block::Quote(line(said.trim())));
        } else if trimmed.starts_with('|') && trimmed.len() > 1 {
            end_paragraph(&mut paragraph, &mut out);
            let mut rows = vec![trimmed.to_string()];
            while let Some(next) = lines.peek().map(|l| l.trim()).filter(|l| l.starts_with('|')) {
                rows.push(next.to_string());
                lines.next();
            }
            out.push(Block::Table(rows));
        } else {
            paragraph.push(trimmed);
        }
    }
    end_paragraph(&mut paragraph, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line as its text and, for each part that is not plain, the part and
    /// its looks: `s`trong, `e`mphasis, struc`k`, `c`ode, and `l`ink.
    fn read(source: &str) -> (String, Vec<(String, String)>) {
        let line = line(source);
        let spans = line
            .spans
            .iter()
            .map(|span| {
                let look = &span.look;
                let mut marks = String::new();
                for (on, mark) in [
                    (look.strong, 's'),
                    (look.emphasis, 'e'),
                    (look.strike, 'k'),
                    (look.code, 'c'),
                    (look.link.is_some(), 'l'),
                ] {
                    if on {
                        marks.push(mark);
                    }
                }
                (line.text[span.range.clone()].to_string(), marks)
            })
            .collect();
        (line.text, spans)
    }

    fn spans(source: &str) -> Vec<(String, String)> {
        read(source).1
    }

    fn pair(text: &str, marks: &str) -> (String, String) {
        (text.to_string(), marks.to_string())
    }

    #[test]
    fn words_keep_their_marks_out_of_the_text_and_in_their_looks() {
        assert_eq!(read("plain words"), ("plain words".into(), vec![]));
        assert_eq!(
            read("a **strong** and *soft* word"),
            ("a strong and soft word".into(), vec![pair("strong", "s"), pair("soft", "e")])
        );
        assert_eq!(spans("__strong__ and _soft_"), [pair("strong", "s"), pair("soft", "e")]);
        assert_eq!(spans("~~gone~~"), [pair("gone", "k")]);
        // One look inside another has both.
        assert_eq!(spans("**a *b* c**"), [pair("a ", "s"), pair("b", "se"), pair(" c", "s")]);
        assert_eq!(spans("***both***"), [pair("both", "se")]);
    }

    #[test]
    fn code_holds_what_is_in_it_as_it_is() {
        assert_eq!(read("run `ls -la` now"), ("run ls -la now".into(), vec![pair("ls -la", "c")]));
        assert_eq!(spans("`a **b** _c_`"), [pair("a **b** _c_", "c")]);
        // Two ticks hold a tick, and a space each side is the fence's.
        assert_eq!(spans("`` a`b ``"), [pair("a`b", "c")]);
        assert_eq!(spans("**`bold code`**"), [pair("bold code", "sc")]);
    }

    #[test]
    fn links_keep_their_words_and_where_they_go() {
        let read = line("see [the guide](https://example.org/a) or https://example.org/b.");
        assert_eq!(read.text, "see the guide or https://example.org/b.");
        let links: Vec<(&str, &str)> = read
            .spans
            .iter()
            .map(|span| (&read.text[span.range.clone()], span.look.link.as_deref().unwrap()))
            .collect();
        assert_eq!(
            links,
            [("the guide", "https://example.org/a"), ("https://example.org/b", "https://example.org/b")]
        );
        // Brackets that are no link are brackets.
        assert_eq!(super::line("an [aside] and a[0](x y)").spans, []);
    }

    #[test]
    fn a_mark_that_is_not_one_stays_as_it_was_typed() {
        for plain in [
            "2 * 3 * 4",
            "a_file_name.rs",
            "snake_case and more_snake",
            "an unclosed **strong",
            "an unclosed `tick",
            "a lone ~ and * and _",
            "5 ~ 6",
        ] {
            assert_eq!(read(plain), (plain.to_string(), vec![]), "{plain}");
        }
        assert_eq!(read(r"\*not soft\* and \`not code\`").0, "*not soft* and `not code`");
        assert_eq!(read("**unclosed and *closed*"), ("**unclosed and closed".into(), vec![pair("closed", "e")]));
    }

    #[test]
    fn an_answer_reads_as_its_blocks() {
        let read = blocks(
            "# Title\n\nFirst line\nof one paragraph.\n\n## Next ##\n- one\n- two\n  more of two\n  - inner\n1. first\n2) second\n\n> quoted\n> again\n\n---\n\n| a | b |\n|---|---|\n| 1 | 2 |\nlast",
        );
        let kinds: Vec<String> = read
            .iter()
            .map(|block| match block {
                Block::Paragraph(l) => format!("p:{}", l.text),
                Block::Heading { level, line } => format!("h{level}:{}", line.text),
                Block::Code { language, text } => format!("code:{language:?}:{text}"),
                Block::List(items) => format!(
                    "list:{}",
                    items
                        .iter()
                        .map(|i| format!("{}{} {}", "  ".repeat(i.depth), i.marker, i.line.text))
                        .collect::<Vec<_>>()
                        .join("/")
                ),
                Block::Quote(l) => format!("q:{}", l.text),
                Block::Table(rows) => format!("table:{}", rows.len()),
                Block::Rule => "rule".into(),
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "h1:Title",
                "p:First line of one paragraph.",
                "h2:Next",
                "list:• one/• two more of two/  • inner/1. first/2. second",
                "q:quoted again",
                "rule",
                "table:3",
                "p:last",
            ]
        );
    }

    #[test]
    fn fenced_code_is_kept_as_written_and_reads_as_code_before_it_is_closed() {
        let read = blocks("Run this:\n```sh\nls -la\n\n  **not strong**\n```\nDone.");
        assert_eq!(
            read,
            [
                Block::Paragraph(line("Run this:")),
                Block::Code {
                    language: Some("sh".into()),
                    text: "ls -la\n\n  **not strong**".into()
                },
                Block::Paragraph(line("Done.")),
            ]
        );
        // An answer still being written: the fence is not closed yet.
        assert_eq!(
            blocks("```\nfn main() {"),
            [Block::Code { language: None, text: "fn main() {".into() }]
        );
        // A longer fence holds a shorter one, and tildes fence too.
        assert_eq!(
            blocks("````md\n```\ninner\n```\n````\n~~~\ntilde\n~~~"),
            [
                Block::Code { language: Some("md".into()), text: "```\ninner\n```".into() },
                Block::Code { language: None, text: "tilde".into() },
            ]
        );
    }

    #[test]
    fn what_looks_like_a_block_and_is_not_one_is_a_paragraph() {
        assert_eq!(blocks("#hashtag"), [Block::Paragraph(line("#hashtag"))]);
        assert_eq!(blocks("-not a list"), [Block::Paragraph(line("-not a list"))]);
        assert_eq!(blocks("1.5 is a number"), [Block::Paragraph(line("1.5 is a number"))]);
        assert_eq!(blocks("a line\n---"), [Block::Paragraph(line("a line ---"))]);
        assert_eq!(blocks(""), []);
        assert_eq!(blocks("\n\n  \n"), []);
    }
}
