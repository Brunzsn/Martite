//! A small group-based pretty-printing document algebra.
//!
//! The formatter builds a `Doc` tree and the renderer decides, for every
//! `Group`, whether its contents fit on the current line. Widths are measured
//! in Unicode scalar values, as required by section 14 of the language rules.

use std::collections::VecDeque;

#[derive(Debug, Clone)]
pub enum Doc {
    Nil,
    Text(String),
    Concat(Vec<Doc>),
    /// A space when flat, a newline when broken.
    Line,
    /// Nothing when flat, a newline when broken.
    SoftLine,
    /// Always a newline. Enclosing groups are forced to break.
    HardLine,
    /// Exactly one empty line between the surrounding content.
    BlankLine,
    Group(Box<Doc>),
    /// Render the contents with one additional indentation level.
    Indent(Box<Doc>),
    /// Render the contents in flat mode regardless of the surrounding group.
    Flat(Box<Doc>),
    /// Choose `flat` when it fits, otherwise `broken`.
    Choice {
        flat: Box<Doc>,
        broken: Box<Doc>,
    },
    /// Emit `broken` when the surrounding group is broken, `flat` otherwise.
    IfBreak {
        broken: Box<Doc>,
        flat: Box<Doc>,
    },
    /// Defer text until the end of the current line.
    LineSuffix(String),
    /// Force every enclosing group to break.
    BreakParent,
}

impl Doc {
    pub fn text(text: impl Into<String>) -> Self {
        Doc::Text(text.into())
    }

    pub fn concat(docs: impl IntoIterator<Item = Doc>) -> Self {
        let mut out = Vec::new();
        for doc in docs {
            match doc {
                Doc::Nil => {}
                Doc::Concat(inner) => out.extend(inner),
                other => out.push(other),
            }
        }
        match out.len() {
            0 => Doc::Nil,
            1 => out.pop().unwrap(),
            _ => Doc::Concat(out),
        }
    }

    pub fn indent(self) -> Self {
        Doc::Indent(Box::new(self))
    }

    pub fn append(self, other: Doc) -> Self {
        Doc::concat([self, other])
    }

    pub fn has_forced_break(&self) -> bool {
        match self {
            Doc::HardLine | Doc::BlankLine | Doc::BreakParent => true,
            Doc::Nil | Doc::Text(_) | Doc::Line | Doc::SoftLine | Doc::LineSuffix(_) => false,
            Doc::Concat(docs) => docs.iter().any(Doc::has_forced_break),
            Doc::Group(doc) | Doc::Indent(doc) | Doc::Flat(doc) => doc.has_forced_break(),
            Doc::Choice { flat, .. } => flat.has_forced_break(),
            Doc::IfBreak { broken, flat } => broken.has_forced_break() && flat.has_forced_break(),
        }
    }

    /// Width of the document when rendered flat, or `None` when it contains a
    /// construct that cannot be flattened.
    pub fn flat_width(&self) -> Option<usize> {
        Some(match self {
            Doc::Nil => 0,
            Doc::Text(text) => text.chars().count(),
            Doc::Concat(docs) => docs
                .iter()
                .try_fold(0usize, |total, doc| Some(total + doc.flat_width()?))?,
            Doc::Line => 1,
            Doc::SoftLine => 0,
            Doc::HardLine | Doc::BlankLine | Doc::BreakParent => return None,
            Doc::Group(doc) | Doc::Indent(doc) | Doc::Flat(doc) => doc.flat_width()?,
            Doc::Choice { flat, .. } => flat.flat_width()?,
            Doc::IfBreak { flat, .. } => flat.flat_width()?,
            Doc::LineSuffix(text) => text.chars().count() + 1,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Flat,
    Break,
}

struct Command<'a> {
    indent: usize,
    mode: Mode,
    doc: &'a Doc,
}

pub struct Renderer {
    width: usize,
    indent_width: usize,
    eol: &'static str,
    out: String,
    column: usize,
    line_start: bool,
    pending_indent: usize,
    line_suffixes: Vec<String>,
    saw_content: bool,
}

impl Renderer {
    pub fn new(width: usize, indent_width: usize, eol: &'static str) -> Self {
        Self {
            width,
            indent_width,
            eol,
            out: String::new(),
            column: 0,
            line_start: true,
            pending_indent: 0,
            line_suffixes: Vec::new(),
            saw_content: false,
        }
    }

    pub fn render(mut self, doc: &Doc) -> String {
        let mut stack: Vec<Command<'_>> = vec![Command {
            indent: 0,
            mode: Mode::Break,
            doc,
        }];
        while let Some(command) = stack.pop() {
            match command.doc {
                Doc::Nil => {}
                Doc::Text(text) => self.write_text(text, command.indent),
                Doc::Concat(docs) => {
                    for doc in docs.iter().rev() {
                        stack.push(Command {
                            indent: command.indent,
                            mode: command.mode,
                            doc,
                        });
                    }
                }
                Doc::Line => {
                    if command.mode == Mode::Flat {
                        self.write_text(" ", command.indent);
                    } else {
                        self.newline(command.indent);
                    }
                }
                Doc::SoftLine => {
                    if command.mode == Mode::Break {
                        self.newline(command.indent);
                    }
                }
                Doc::HardLine => self.newline(command.indent),
                Doc::BlankLine => self.blank_line(command.indent),
                Doc::Group(doc) => {
                    let next = Command {
                        indent: command.indent,
                        mode: Mode::Flat,
                        doc,
                    };
                    let fits = !doc.has_forced_break()
                        && self.fits(next, &stack)
                        && !doc.has_forced_break();
                    stack.push(Command {
                        indent: command.indent,
                        mode: if fits { Mode::Flat } else { Mode::Break },
                        doc,
                    });
                }
                Doc::Indent(doc) => stack.push(Command {
                    indent: command.indent + self.indent_width,
                    mode: command.mode,
                    doc,
                }),
                Doc::Flat(doc) => stack.push(Command {
                    indent: command.indent,
                    mode: Mode::Flat,
                    doc,
                }),
                Doc::Choice { flat, broken } => {
                    let next = Command {
                        indent: command.indent,
                        mode: Mode::Flat,
                        doc: flat,
                    };
                    let fits = !flat.has_forced_break() && self.fits(next, &stack);
                    stack.push(Command {
                        indent: command.indent,
                        mode: if fits { Mode::Flat } else { Mode::Break },
                        doc: if fits { flat } else { broken },
                    });
                }
                Doc::IfBreak { broken, flat } => stack.push(Command {
                    indent: command.indent,
                    mode: command.mode,
                    doc: if command.mode == Mode::Break {
                        broken
                    } else {
                        flat
                    },
                }),
                Doc::LineSuffix(text) => self.line_suffixes.push(text.clone()),
                Doc::BreakParent => {}
            }
        }
        self.finish()
    }

    fn fits(&self, next: Command<'_>, rest: &[Command<'_>]) -> bool {
        let mut remaining = self.width.saturating_sub(
            self.column
                + if self.line_start {
                    self.pending_indent
                } else {
                    0
                },
        );
        let mut pending: Vec<Command<'_>> = Vec::new();
        pending.push(next);
        for command in rest.iter().rev() {
            pending.push(Command {
                indent: command.indent,
                mode: command.mode,
                doc: command.doc,
            });
        }
        if !self.line_suffixes.is_empty() {
            let suffix: usize = self
                .line_suffixes
                .iter()
                .map(|text| text.chars().count() + 1)
                .sum();
            if suffix > remaining {
                return false;
            }
            remaining -= suffix;
        }
        let mut queue: VecDeque<Command<'_>> = VecDeque::new();
        for command in pending {
            queue.push_back(command);
        }
        while let Some(command) = queue.pop_front() {
            match command.doc {
                Doc::Nil | Doc::BreakParent => {}
                Doc::Text(text) => {
                    let width = text.chars().count();
                    if width > remaining {
                        return false;
                    }
                    remaining -= width;
                }
                Doc::Concat(docs) => {
                    for doc in docs.iter().rev() {
                        queue.push_front(Command {
                            indent: command.indent,
                            mode: command.mode,
                            doc,
                        });
                    }
                }
                Doc::Line => {
                    if command.mode == Mode::Break {
                        return true;
                    }
                    if remaining == 0 {
                        return false;
                    }
                    remaining -= 1;
                }
                Doc::SoftLine => {
                    if command.mode == Mode::Break {
                        return true;
                    }
                }
                Doc::HardLine | Doc::BlankLine => return true,
                Doc::Group(doc) => queue.push_front(Command {
                    indent: command.indent,
                    mode: command.mode,
                    doc,
                }),
                Doc::Indent(doc) => queue.push_front(Command {
                    indent: command.indent + self.indent_width,
                    mode: command.mode,
                    doc,
                }),
                Doc::Flat(doc) => queue.push_front(Command {
                    indent: command.indent,
                    mode: Mode::Flat,
                    doc,
                }),
                Doc::Choice { flat, .. } => queue.push_front(Command {
                    indent: command.indent,
                    mode: Mode::Flat,
                    doc: flat,
                }),
                Doc::IfBreak { flat, .. } => queue.push_front(Command {
                    indent: command.indent,
                    mode: command.mode,
                    doc: flat,
                }),
                Doc::LineSuffix(text) => {
                    let width = text.chars().count() + 1;
                    if width > remaining {
                        return false;
                    }
                    remaining -= width;
                }
            }
        }
        true
    }

    fn write_text(&mut self, text: &str, indent: usize) {
        if text.is_empty() {
            return;
        }
        if self.line_start {
            let spaces = indent;
            if spaces > 0 {
                self.out.extend(std::iter::repeat_n(' ', spaces));
                self.column = spaces;
            }
            self.line_start = false;
        }
        self.out.push_str(text);
        self.column += text.chars().count();
        self.saw_content = true;
    }

    fn flush_line_suffixes(&mut self) {
        if self.line_suffixes.is_empty() {
            return;
        }
        let suffixes = std::mem::take(&mut self.line_suffixes);
        for suffix in suffixes {
            if !self.line_start && self.column > 0 {
                self.out.push(' ');
                self.column += 1;
            }
            self.out.push_str(&suffix);
            self.column += suffix.chars().count();
        }
    }

    fn newline(&mut self, indent: usize) {
        self.flush_line_suffixes();
        if self.line_start {
            self.pending_indent = indent;
            return;
        }
        self.out.push_str(self.eol);
        self.line_start = true;
        self.pending_indent = indent;
        self.column = 0;
    }

    fn blank_line(&mut self, indent: usize) {
        self.newline(indent);
        if !self.line_start {
            return;
        }
        if self.out.is_empty() || self.out.ends_with(self.eol) {
            // `newline` either emitted one newline or was a no-op at line
            // start. In both cases exactly one extra newline makes one blank
            // line, but never stack more than one.
            if !self.out.ends_with(&format!("{}{}", self.eol, self.eol)) {
                self.out.push_str(self.eol);
            }
        }
        self.pending_indent = indent;
    }

    fn finish(mut self) -> String {
        self.flush_line_suffixes();
        while self.out.ends_with('\n') || self.out.ends_with('\r') {
            self.out.pop();
        }
        if self.saw_content || !self.out.is_empty() {
            self.out.push_str(self.eol);
        }
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(doc: &Doc, width: usize) -> String {
        Renderer::new(width, 4, "\n").render(doc)
    }

    #[test]
    fn groups_break_only_when_needed() {
        let doc = Doc::Group(Box::new(Doc::concat([
            Doc::text("call("),
            Doc::concat([
                Doc::SoftLine,
                Doc::text("first,"),
                Doc::Line,
                Doc::text("second"),
            ])
            .indent(),
            Doc::SoftLine,
            Doc::text(")"),
        ])));
        assert_eq!(render(&doc, 80), "call(first, second)\n");
        assert_eq!(render(&doc, 10), "call(\n    first,\n    second\n)\n");
    }

    #[test]
    fn width_counts_scalar_values() {
        let doc = Doc::text("éééééééééé");
        assert_eq!(render(&doc, 10), "éééééééééé\n");
    }

    #[test]
    fn nested_group_stays_flat_inside_broken_parent() {
        let entry = Doc::Group(Box::new(Doc::concat([
            Doc::text("a:"),
            Doc::concat([Doc::Line, Doc::text("1")]).indent(),
        ])));
        let outer = Doc::Group(Box::new(Doc::concat([
            Doc::text("{"),
            Doc::concat([
                Doc::SoftLine,
                entry,
                Doc::text(","),
                Doc::LineSuffix("// c".into()),
                Doc::BreakParent,
                Doc::Line,
                Doc::text("b: 2"),
            ])
            .indent(),
            Doc::SoftLine,
            Doc::text("}"),
        ])));
        assert_eq!(render(&outer, 100), "{\n    a: 1, // c\n    b: 2\n}\n");
    }
}
