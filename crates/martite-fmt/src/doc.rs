//! A small group-based pretty-printing document algebra.
//!
//! The formatter builds a `Doc` tree and the renderer decides, for every
//! `Group`, whether its contents fit on the current line. Widths are measured
//! in Unicode scalar values, as required by section 14 of the language rules.

use std::borrow::Cow;

/// One piece of text with its cached scalar width.
#[derive(Debug, Clone)]
pub struct Text {
    text: Cow<'static, str>,
    width: usize,
}

impl Text {
    fn new(text: impl Into<Cow<'static, str>>) -> Self {
        let text = text.into();
        let width = text.chars().count();
        Self { text, width }
    }

    fn as_str(&self) -> &str {
        &self.text
    }

    fn width(&self) -> usize {
        self.width
    }
}

impl From<&'static str> for Text {
    fn from(text: &'static str) -> Self {
        Self::new(text)
    }
}

impl From<String> for Text {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

/// Cached layout information for a document subtree.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    flat_width: Option<usize>,
    forced_break: bool,
}

impl Layout {
    const NIL: Self = Self {
        flat_width: Some(0),
        forced_break: false,
    };

    const fn width(width: usize) -> Self {
        Self {
            flat_width: Some(width),
            forced_break: false,
        }
    }

    const fn forced() -> Self {
        Self {
            flat_width: None,
            forced_break: true,
        }
    }

    fn join(self, other: Self) -> Self {
        Self {
            flat_width: match (self.flat_width, other.flat_width) {
                (Some(left), Some(right)) => Some(left + right),
                _ => None,
            },
            forced_break: self.forced_break || other.forced_break,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Doc {
    Nil,
    Text(Text),
    Concat {
        docs: Vec<Doc>,
        layout: Layout,
    },
    /// A space when flat, a newline when broken.
    Line,
    /// Nothing when flat, a newline when broken.
    SoftLine,
    /// Always a newline. Enclosing groups are forced to break.
    HardLine,
    /// Exactly one empty line between the surrounding content.
    BlankLine,
    Group {
        doc: Box<Doc>,
        layout: Layout,
    },
    /// Render the contents with one additional indentation level.
    Indent(Box<Doc>),
    /// Render the contents in flat mode regardless of the surrounding group.
    Flat(Box<Doc>),
    /// Choose `flat` when it fits, otherwise `broken`.
    Choice {
        flat: Box<Doc>,
        broken: Box<Doc>,
        layout: Layout,
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
    pub fn text(text: impl Into<Text>) -> Self {
        Doc::Text(text.into())
    }

    pub fn group(doc: Doc) -> Self {
        let layout = doc.layout();
        Doc::Group {
            doc: Box::new(doc),
            layout,
        }
    }

    pub fn choice(flat: Doc, broken: Doc) -> Self {
        let layout = flat.layout();
        Doc::Choice {
            flat: Box::new(flat),
            broken: Box::new(broken),
            layout,
        }
    }

    pub fn concat(docs: impl IntoIterator<Item = Doc>) -> Self {
        let mut out = Doc::Nil;
        for doc in docs {
            out = out.append(doc);
        }
        out
    }

    pub fn indent(self) -> Self {
        Doc::Indent(Box::new(self))
    }

    pub fn append(self, other: Doc) -> Self {
        if matches!(other, Doc::Nil) {
            return self;
        }
        let other_layout = other.layout();
        match self {
            Doc::Nil => other,
            Doc::Concat {
                mut docs,
                mut layout,
            } => {
                match other {
                    Doc::Concat {
                        docs: mut inner, ..
                    } => docs.append(&mut inner),
                    other => docs.push(other),
                }
                layout = layout.join(other_layout);
                Doc::Concat { docs, layout }
            }
            other_self => {
                let layout = other_self.layout().join(other_layout);
                Doc::Concat {
                    docs: vec![other_self, other],
                    layout,
                }
            }
        }
    }

    fn layout(&self) -> Layout {
        match self {
            Doc::Nil => Layout::NIL,
            Doc::Text(text) => Layout::width(text.width()),
            Doc::Concat { layout, .. } | Doc::Group { layout, .. } | Doc::Choice { layout, .. } => {
                *layout
            }
            Doc::Line => Layout::width(1),
            Doc::SoftLine => Layout::NIL,
            Doc::HardLine | Doc::BlankLine | Doc::BreakParent => Layout::forced(),
            Doc::Indent(doc) | Doc::Flat(doc) => doc.layout(),
            Doc::IfBreak { broken, flat } => Layout {
                flat_width: flat.layout().flat_width,
                forced_break: broken.layout().forced_break && flat.layout().forced_break,
            },
            Doc::LineSuffix(text) => Layout::width(text.chars().count() + 1),
        }
    }

    /// Width of the document when rendered flat, or `None` when it contains a
    /// construct that cannot be flattened.
    pub fn flat_width(&self) -> Option<usize> {
        self.layout().flat_width
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Flat,
    Break,
}

#[derive(Debug, Clone, Copy)]
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
    line_suffix_width: usize,
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
            line_suffix_width: 0,
            saw_content: false,
        }
    }

    pub fn render<'a>(mut self, doc: &'a Doc) -> String {
        let mut stack: Vec<Command<'a>> = vec![Command {
            indent: 0,
            mode: Mode::Break,
            doc,
        }];
        let mut scratch = Vec::new();
        while let Some(command) = stack.pop() {
            match command.doc {
                Doc::Nil => {}
                Doc::Text(text) => self.write_text(text.as_str(), text.width(), command.indent),
                Doc::Concat { docs, .. } => {
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
                        self.write_text(" ", 1, command.indent);
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
                Doc::Group { doc, layout } => {
                    let next = Command {
                        indent: command.indent,
                        mode: Mode::Flat,
                        doc,
                    };
                    let fits = !layout.forced_break && self.fits(next, &stack, &mut scratch);
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
                Doc::Choice {
                    flat,
                    broken,
                    layout,
                } => {
                    let next = Command {
                        indent: command.indent,
                        mode: Mode::Flat,
                        doc: flat,
                    };
                    let fits = !layout.forced_break && self.fits(next, &stack, &mut scratch);
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
                Doc::LineSuffix(text) => {
                    self.line_suffix_width += text.chars().count() + 1;
                    self.line_suffixes.push(text.clone());
                }
                Doc::BreakParent => {}
            }
        }
        self.finish()
    }

    fn fits<'a>(
        &self,
        next: Command<'a>,
        rest: &[Command<'a>],
        scratch: &mut Vec<Command<'a>>,
    ) -> bool {
        let mut remaining = self.width.saturating_sub(
            self.column
                + if self.line_start {
                    self.pending_indent
                } else {
                    0
                },
        );
        if self.line_suffix_width > remaining {
            return false;
        }
        remaining -= self.line_suffix_width;

        scratch.clear();
        scratch.push(next);
        let mut rest_index = rest.len();
        loop {
            let command = match scratch.pop() {
                Some(command) => command,
                None => {
                    if rest_index == 0 {
                        return true;
                    }
                    rest_index -= 1;
                    rest[rest_index]
                }
            };

            // A flat subtree has an exact cached width, so a whole command can
            // be measured without expanding it.
            if command.mode == Mode::Flat
                && let Some(width) = command.doc.flat_width()
            {
                if width > remaining {
                    return false;
                }
                remaining -= width;
                continue;
            }

            match command.doc {
                Doc::Nil | Doc::BreakParent => {}
                Doc::Text(text) => {
                    let width = text.width();
                    if width > remaining {
                        return false;
                    }
                    remaining -= width;
                }
                Doc::Concat { docs, .. } => {
                    for doc in docs.iter().rev() {
                        scratch.push(Command {
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
                Doc::Group { doc, .. } => scratch.push(Command {
                    indent: command.indent,
                    mode: command.mode,
                    doc,
                }),
                Doc::Indent(doc) => scratch.push(Command {
                    indent: command.indent + self.indent_width,
                    mode: command.mode,
                    doc,
                }),
                Doc::Flat(doc) => scratch.push(Command {
                    indent: command.indent,
                    mode: Mode::Flat,
                    doc,
                }),
                Doc::Choice { flat, .. } => scratch.push(Command {
                    indent: command.indent,
                    mode: Mode::Flat,
                    doc: flat,
                }),
                Doc::IfBreak { flat, .. } => scratch.push(Command {
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
    }

    fn write_text(&mut self, text: &str, width: usize, indent: usize) {
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
        self.column += width;
        self.saw_content = true;
    }

    fn flush_line_suffixes(&mut self) {
        if self.line_suffixes.is_empty() {
            return;
        }
        let suffixes = std::mem::take(&mut self.line_suffixes);
        self.line_suffix_width = 0;
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
        let ends_with_eol = self.out.ends_with(self.eol);
        if self.out.is_empty() || ends_with_eol {
            // `newline` either emitted one newline or was a no-op at line
            // start. In both cases exactly one extra newline makes one blank
            // line, but never stack more than one.
            if !ends_with_eol || !self.out[..self.out.len() - self.eol.len()].ends_with(self.eol) {
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
        let doc = Doc::group(Doc::concat([
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
        ]));
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
        let entry = Doc::group(Doc::concat([
            Doc::text("a:"),
            Doc::concat([Doc::Line, Doc::text("1")]).indent(),
        ]));
        let outer = Doc::group(Doc::concat([
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
        ]));
        assert_eq!(render(&outer, 100), "{\n    a: 1, // c\n    b: 2\n}\n");
    }

    #[test]
    fn cached_layout_matches_flat_rendering() {
        let doc = Doc::concat([
            Doc::text("ab"),
            Doc::Line,
            Doc::text("c"),
            Doc::SoftLine,
            Doc::text("d"),
        ]);
        assert_eq!(doc.flat_width(), Some(5));
        assert!(!doc.layout().forced_break);

        let doc = doc.append(Doc::HardLine);
        assert_eq!(doc.flat_width(), None);
        assert!(doc.layout().forced_break);
    }

    #[test]
    fn static_text_borrows_its_source() {
        let doc = Doc::text("static");
        let Doc::Text(text) = doc else {
            panic!("expected text");
        };
        assert!(matches!(text.text, Cow::Borrowed(_)));
        assert_eq!(text.width(), 6);
    }
}
