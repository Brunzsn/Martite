//! Canonical formatter for Martite.

mod doc;
mod format;
mod normalize;
mod verify;

use martite_base::{Code, Diagnostic, LoadLimits, normalize_source};
use martite_config::FmtConfig;
use martite_syntax::{
    Program, lex, parse_interface_normalized_with_tokens, parse_source_normalized_with_tokens,
};

pub use verify::{comment_multiset, structurally_equal};

#[derive(Debug, Clone)]
pub struct FormatOutput {
    /// `None` when the input has a syntax error (E001–E003) and must be left
    /// untouched.
    pub text: Option<String>,
    pub diagnostics: Vec<Diagnostic>,
}

impl FormatOutput {
    pub fn has_syntax_error(&self) -> bool {
        self.diagnostics.iter().any(|diagnostic| {
            matches!(diagnostic.code, Code::E001 | Code::E002 | Code::E003)
                && diagnostic.severity == martite_base::Severity::Error
        })
    }
}

/// Parse and format a `.mt` source file.
pub fn format_source(source: &str, config: &FmtConfig) -> FormatOutput {
    format_mode(source, config, false)
}

/// Parse and format an interface (`.mti`) file.
pub fn format_interface(source: &str, config: &FmtConfig) -> FormatOutput {
    format_mode(source, config, true)
}

fn format_mode(source: &str, config: &FmtConfig, interface: bool) -> FormatOutput {
    let source = normalize_source(source);
    let (parsed, tokens) = if interface {
        parse_interface_normalized_with_tokens(source.as_ref(), LoadLimits::default())
    } else {
        parse_source_normalized_with_tokens(source.as_ref(), LoadLimits::default())
    };
    let has_syntax_error = parsed.diagnostics.iter().any(|diagnostic| {
        matches!(diagnostic.code, Code::E001 | Code::E002 | Code::E003)
            && diagnostic.severity == martite_base::Severity::Error
    });
    if has_syntax_error {
        return FormatOutput {
            text: None,
            diagnostics: parsed
                .diagnostics
                .into_iter()
                .filter(|diagnostic| {
                    matches!(diagnostic.code, Code::E001 | Code::E002 | Code::E003)
                })
                .collect(),
        };
    }
    let text = format::format_program(source.as_ref(), &parsed.program, tokens, config);
    FormatOutput {
        text: Some(text),
        diagnostics: Vec::new(),
    }
}

/// Format an already parsed program. `source` must already be normalized.
pub fn format_program(source: &str, program: &Program, config: &FmtConfig) -> String {
    let lexed = lex(source, LoadLimits::default());
    format::format_program(source, program, lexed.tokens, config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worked_example() {
        let source = r#"use app.status
use app.editor

on saved(mut self, doc: Doc, kind: SaveKind) -> Result[(), str] {
  if kind == SaveKind.Auto {
        return Ok(()) }
    let words = editor.text(doc).split(" ").filter(fn(w) => w != "").map(fn(w) => w.trim()).filter(fn(w) => w.len() > 0).len()
    self.saves += 1
    status.show("{words} words after {self.saves} saves")
}
"#;
        let output = format_source(source, &FmtConfig::default());
        println!("{}", output.text.as_deref().unwrap_or("<syntax error>"));
        assert_eq!(
            output.text.unwrap(),
            r#"use app.editor
use app.status

on saved(mut self, doc: Doc, kind: SaveKind) -> Result[(), str] {
    if kind == SaveKind.Auto { return Ok(()) }
    let words = editor.text(doc)
        .split(" ")
        .filter(fn(w) => w != "")
        .map(fn(w) => w.trim())
        .filter(fn(w) => w.len() > 0)
        .len()
    self.saves += 1
    status.show("{words} words after {self.saves} saves")
}
"#
        );
    }
}
