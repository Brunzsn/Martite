//! Literal and comment normalization for the formatter.

use martite_syntax::{Comment, TokenKind};

/// Normalize one numeric literal source slice without changing its value.
pub fn number(raw: &str) -> String {
    let (sign, body) = match raw.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", raw),
    };
    let mut out = String::from(sign);
    if let Some(rest) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        out.push_str("0x");
        out.push_str(&rest.to_ascii_uppercase());
        return out;
    }
    if let Some(rest) = body.strip_prefix("0b").or_else(|| body.strip_prefix("0B")) {
        out.push_str("0b");
        out.push_str(rest);
        return out;
    }
    // Float exponents are the only other place where case is normalized.
    if let Some(index) = body.find(['e', 'E']) {
        out.push_str(&body[..index]);
        out.push('e');
        out.push_str(&body[index + 1..]);
        return out;
    }
    out.push_str(body);
    out
}

/// Normalize a comment's body and marker. `///` comments stay doc comments;
/// `// allow(...)` suppressions get the canonical spelling.
pub fn comment(comment: &Comment) -> String {
    let mut raw = comment.text.as_str();
    if comment.doc {
        raw = raw.strip_prefix('/').unwrap_or(raw);
    }
    let text = if let Some(ids) = parse_allow(raw.trim_start()) {
        format!(" allow({})", ids.join(", "))
    } else if raw.trim_start().is_empty() {
        String::new()
    } else if raw.starts_with('/') {
        raw.to_string()
    } else {
        format!(" {}", raw.trim_start())
    };
    let marker = if comment.doc { "///" } else { "//" };
    format!("{marker}{text}")
}

fn parse_allow(text: &str) -> Option<Vec<&str>> {
    let rest = text.strip_prefix("allow")?;
    let rest = rest.trim_start();
    let close = rest.rfind(')')?;
    if !rest.starts_with('(') {
        return None;
    }
    let content = &rest[1..close];
    let ids: Vec<&str> = content
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .collect();
    if ids.is_empty() || rest[close + 1..].trim() != "" {
        return None;
    }
    Some(ids)
}

/// Return the source spelling of a literal token, falling back to a value
/// rendering when the span is unavailable.
pub fn literal_text(source: &str, span: martite_base::Span, fallback: &str) -> String {
    let start = span.start as usize;
    let end = span.end as usize;
    if start <= end
        && end <= source.len()
        && source.is_char_boundary(start)
        && source.is_char_boundary(end)
    {
        let text = &source[start..end];
        if !text.is_empty() {
            return text.to_string();
        }
    }
    fallback.to_string()
}

pub fn token_text(kind: &TokenKind) -> Option<&'static str> {
    kind.keyword_text().or_else(|| match kind {
        TokenKind::Comma => Some(","),
        TokenKind::Colon => Some(":"),
        TokenKind::Dot => Some("."),
        TokenKind::DotDot => Some(".."),
        TokenKind::DotDotEq => Some("..="),
        TokenKind::Question => Some("?"),
        TokenKind::QuestionQuestion => Some("??"),
        TokenKind::Arrow => Some("->"),
        TokenKind::FatArrow => Some("=>"),
        TokenKind::Pipe => Some("|"),
        TokenKind::Underscore => Some("_"),
        TokenKind::Plus => Some("+"),
        TokenKind::Minus => Some("-"),
        TokenKind::Star => Some("*"),
        TokenKind::Slash => Some("/"),
        TokenKind::Percent => Some("%"),
        TokenKind::Eq => Some("="),
        TokenKind::EqEq => Some("=="),
        TokenKind::NotEq => Some("!="),
        TokenKind::Lt => Some("<"),
        TokenKind::Le => Some("<="),
        TokenKind::Gt => Some(">"),
        TokenKind::Ge => Some(">="),
        TokenKind::PlusEq => Some("+="),
        TokenKind::MinusEq => Some("-="),
        TokenKind::StarEq => Some("*="),
        TokenKind::SlashEq => Some("/="),
        TokenKind::PercentEq => Some("%="),
        TokenKind::LParen => Some("("),
        TokenKind::RParen => Some(")"),
        TokenKind::LBracket => Some("["),
        TokenKind::RBracket => Some("]"),
        TokenKind::LBrace => Some("{"),
        TokenKind::RBrace => Some("}"),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use martite_base::Span;

    fn make_comment(text: &str, doc: bool) -> Comment {
        Comment {
            span: Span::empty(0),
            text: text.into(),
            doc,
        }
    }

    #[test]
    fn normalizes_numbers() {
        assert_eq!(number("0xab_cd"), "0xAB_CD");
        assert_eq!(number("0XAB"), "0xAB");
        assert_eq!(number("0B10"), "0b10");
        assert_eq!(number("1E9"), "1e9");
        assert_eq!(number("-0xff"), "-0xFF");
        assert_eq!(number("1_000"), "1_000");
    }

    #[test]
    fn normalizes_comments() {
        assert_eq!(comment(&make_comment("foo", false)), "// foo");
        assert_eq!(comment(&make_comment(" foo", false)), "// foo");
        assert_eq!(comment(&make_comment("", false)), "//");
        assert_eq!(comment(&make_comment(" /path", false)), "// /path");
        assert_eq!(comment(&make_comment("   foo", false)), "// foo");
        assert_eq!(comment(&make_comment("//foo", true)), "////foo");
        assert_eq!(comment(&make_comment(" foo", true)), "/// foo");
        assert_eq!(
            comment(&make_comment(" allow( L001,L002 )", false)),
            "// allow(L001, L002)"
        );
    }
}
