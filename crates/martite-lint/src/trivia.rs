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

/// The span of a keyword plus following horizontal whitespace, so deleting it
/// does not leave `fn f( x)` or `f( x)` behind.
pub fn keyword_removal_span(text: &str, span: Span) -> Span {
    let mut end = (span.end as usize).min(text.len());
    while end < text.len() && matches!(text.as_bytes()[end], b' ' | b'\t') {
        end += 1;
    }
    Span::new(span.start, end as u32)
}

/// The span to remove for an unreachable statement. Prefer the statement's own
/// line, but never absorb text after the statement, which can be an enclosing
/// `}` on a shared line.
pub fn statement_removal_span(text: &str, span: Span) -> Span {
    let start = span.start.min(text.len() as u32) as usize;
    let end = span.end.min(text.len() as u32) as usize;
    let line_start = text[..start]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    let line_end = text[end..]
        .find('\n')
        .map(|offset| end + offset + 1)
        .unwrap_or(text.len());
    let prefix_is_space = text[line_start..start].chars().all(char::is_whitespace);
    let suffix_end = if line_end > end && text.as_bytes()[line_end - 1] == b'\n' {
        line_end - 1
    } else {
        line_end
    };
    let suffix = &text[end..suffix_end];
    let suffix_is_space = suffix.trim().is_empty() || suffix.trim_start().starts_with("//");
    if prefix_is_space && suffix_is_space {
        Span::new(line_start as u32, line_end as u32)
    } else {
        Span::new(start as u32, end as u32)
    }
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
