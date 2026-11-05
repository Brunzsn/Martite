//! Comment trivia helpers shared by the rules and suppression handling.

use martite_base::Span;
use martite_syntax::{Comment, Program};

/// True when the byte range contains only whitespace.
pub fn whitespace_only(text: &str, start: u32, end: u32) -> bool {
    let start = start.min(text.len() as u32) as usize;
    let end = end.min(text.len() as u32) as usize;
    if start > end {
        return false;
    }
    text[start..end].chars().all(char::is_whitespace)
}

/// The start of an item including a preceding `pub` keyword. AST spans start
/// at `fn`, `struct`, `enum`, or `const`, so docs and suppressions written
/// directly above a public item would otherwise appear detached.
pub fn item_start(text: &str, start: u32) -> u32 {
    let start = start.min(text.len() as u32) as usize;
    let before = &text[..start];
    let trimmed = before.trim_end();
    if let Some(prefix) = trimmed.strip_suffix("pub") {
        let boundary = prefix
            .chars()
            .next_back()
            .map_or(true, |ch| ch.is_whitespace() || ch == '{' || ch == '}');
        if boundary {
            return prefix.len() as u32;
        }
    }
    start as u32
}

/// Comments directly before `start`, with only whitespace between them and the
/// element. Blank lines do not detach a comment. Returned in source order.
pub fn preceding_comments<'a>(program: &'a Program, text: &str, start: u32) -> Vec<&'a Comment> {
    let mut out = Vec::new();
    let mut pos = start;
    loop {
        let mut found: Option<&Comment> = None;
        for comment in program.comments.iter().rev() {
            if comment.span.end > pos {
                continue;
            }
            if whitespace_only(text, comment.span.end, pos) {
                found = Some(comment);
            }
            // Earlier comments cannot qualify if this one does not: the gap
            // would contain this comment's text.
            break;
        }
        let Some(comment) = found else {
            break;
        };
        out.push(comment);
        pos = comment.span.start;
    }
    out.reverse();
    out
}

/// The trailing run of `///` comments directly before an element.
pub fn doc_comments<'a>(program: &'a Program, text: &str, start: u32) -> Vec<&'a Comment> {
    let mut out = Vec::new();
    for comment in preceding_comments(program, text, start).into_iter().rev() {
        if comment.doc {
            out.push(comment);
        } else {
            break;
        }
    }
    out.reverse();
    out
}

/// The trailing run of `// allow(...)` comments directly before an element.
pub fn allow_comments<'a>(program: &'a Program, text: &str, start: u32) -> Vec<&'a Comment> {
    let mut out = Vec::new();
    for comment in preceding_comments(program, text, start).into_iter().rev() {
        if !comment.doc && is_allow_comment(comment) {
            out.push(comment);
        } else {
            break;
        }
    }
    out.reverse();
    out
}

pub fn is_allow_comment(comment: &Comment) -> bool {
    !comment.doc && comment.text.trim_start().starts_with("allow")
}

pub fn comments_within<'a>(program: &'a Program, span: Span) -> Vec<&'a Comment> {
    program
        .comments
        .iter()
        .filter(|comment| span.contains(comment.span))
        .collect()
}

/// Find the first occurrence of `name` inside a declaration span.
pub fn find_name_span(text: &str, span: Span, name: &str) -> Span {
    let start = span.start.min(text.len() as u32) as usize;
    let end = span.end.min(text.len() as u32) as usize;
    if start <= end {
        if let Some(offset) = text[start..end].find(name) {
            let at = start + offset;
            return Span::new(at as u32, (at + name.len()) as u32);
        }
    }
    span
}

/// Find an infix operator between two operands.
pub fn operator_span(text: &str, lhs_end: u32, rhs_start: u32, op: &str) -> Span {
    let start = lhs_end.min(text.len() as u32) as usize;
    let end = rhs_start.min(text.len() as u32) as usize;
    if start > end {
        return Span::empty(lhs_end);
    }
    let slice = &text[start..end];
    if op.chars().all(|ch| ch.is_ascii_alphabetic()) {
        let bytes = slice.as_bytes();
        let needle = op.as_bytes();
        let mut index = 0usize;
        while index + needle.len() <= bytes.len() {
            if &bytes[index..index + needle.len()] == needle {
                let before_ok = index == 0
                    || !bytes[index - 1].is_ascii_alphanumeric() && bytes[index - 1] != b'_';
                let after = index + needle.len();
                let after_ok = after == bytes.len()
                    || !bytes[after].is_ascii_alphanumeric() && bytes[after] != b'_';
                if before_ok && after_ok {
                    let at = start + index;
                    return Span::new(at as u32, (at + needle.len()) as u32);
                }
            }
            index += 1;
        }
    } else if let Some(offset) = slice.find(op) {
        let at = start + offset;
        return Span::new(at as u32, (at + op.len()) as u32);
    }
    Span::empty(lhs_end)
}
