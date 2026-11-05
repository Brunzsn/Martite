use std::fmt;

use crate::source::{SourceId, SourceMap, Span};

/// Every compile error and lint rule in edition 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Code {
    E001,
    E002,
    E003,
    E101,
    E102,
    E103,
    E104,
    E105,
    E106,
    E201,
    E202,
    E203,
    E204,
    E205,
    E206,
    E207,
    E208,
    E209,
    E301,
    E302,
    E303,
    E304,
    E401,
    E402,
    E403,
    E404,
    E405,
    E406,
    E407,
    E501,
    E502,
    E503,
    E504,
    E505,
    E506,
    E507,
    E508,
    E509,
    L001,
    L002,
    L003,
    L004,
    L005,
    L006,
    L007,
    L008,
    L009,
    L010,
    L011,
    L012,
    L013,
    L014,
    L015,
    L016,
    L017,
    L018,
    L019,
    L020,
    L021,
    L022,
    L023,
    L024,
    L025,
    L026,
    L027,
    L028,
    L029,
    L030,
    L031,
    L032,
}

impl Code {
    pub const fn as_str(self) -> &'static str {
        match self {
            Code::E001 => "E001",
            Code::E002 => "E002",
            Code::E003 => "E003",
            Code::E101 => "E101",
            Code::E102 => "E102",
            Code::E103 => "E103",
            Code::E104 => "E104",
            Code::E105 => "E105",
            Code::E106 => "E106",
            Code::E201 => "E201",
            Code::E202 => "E202",
            Code::E203 => "E203",
            Code::E204 => "E204",
            Code::E205 => "E205",
            Code::E206 => "E206",
            Code::E207 => "E207",
            Code::E208 => "E208",
            Code::E209 => "E209",
            Code::E301 => "E301",
            Code::E302 => "E302",
            Code::E303 => "E303",
            Code::E304 => "E304",
            Code::E401 => "E401",
            Code::E402 => "E402",
            Code::E403 => "E403",
            Code::E404 => "E404",
            Code::E405 => "E405",
            Code::E406 => "E406",
            Code::E407 => "E407",
            Code::E501 => "E501",
            Code::E502 => "E502",
            Code::E503 => "E503",
            Code::E504 => "E504",
            Code::E505 => "E505",
            Code::E506 => "E506",
            Code::E507 => "E507",
            Code::E508 => "E508",
            Code::E509 => "E509",
            Code::L001 => "L001",
            Code::L002 => "L002",
            Code::L003 => "L003",
            Code::L004 => "L004",
            Code::L005 => "L005",
            Code::L006 => "L006",
            Code::L007 => "L007",
            Code::L008 => "L008",
            Code::L009 => "L009",
            Code::L010 => "L010",
            Code::L011 => "L011",
            Code::L012 => "L012",
            Code::L013 => "L013",
            Code::L014 => "L014",
            Code::L015 => "L015",
            Code::L016 => "L016",
            Code::L017 => "L017",
            Code::L018 => "L018",
            Code::L019 => "L019",
            Code::L020 => "L020",
            Code::L021 => "L021",
            Code::L022 => "L022",
            Code::L023 => "L023",
            Code::L024 => "L024",
            Code::L025 => "L025",
            Code::L026 => "L026",
            Code::L027 => "L027",
            Code::L028 => "L028",
            Code::L029 => "L029",
            Code::L030 => "L030",
            Code::L031 => "L031",
            Code::L032 => "L032",
        }
    }

    pub const fn is_lint(self) -> bool {
        matches!(self.as_str().as_bytes()[0], b'L')
    }

    pub const fn default_severity(self) -> Severity {
        if self.is_lint() {
            Severity::Warning
        } else {
            Severity::Error
        }
    }

    pub fn from_str(s: &str) -> Option<Code> {
        let all = [
            Code::E001,
            Code::E002,
            Code::E003,
            Code::E101,
            Code::E102,
            Code::E103,
            Code::E104,
            Code::E105,
            Code::E106,
            Code::E201,
            Code::E202,
            Code::E203,
            Code::E204,
            Code::E205,
            Code::E206,
            Code::E207,
            Code::E208,
            Code::E209,
            Code::E301,
            Code::E302,
            Code::E303,
            Code::E304,
            Code::E401,
            Code::E402,
            Code::E403,
            Code::E404,
            Code::E405,
            Code::E406,
            Code::E407,
            Code::E501,
            Code::E502,
            Code::E503,
            Code::E504,
            Code::E505,
            Code::E506,
            Code::E507,
            Code::E508,
            Code::E509,
            Code::L001,
            Code::L002,
            Code::L003,
            Code::L004,
            Code::L005,
            Code::L006,
            Code::L007,
            Code::L008,
            Code::L009,
            Code::L010,
            Code::L011,
            Code::L012,
            Code::L013,
            Code::L014,
            Code::L015,
            Code::L016,
            Code::L017,
            Code::L018,
            Code::L019,
            Code::L020,
            Code::L021,
            Code::L022,
            Code::L023,
            Code::L024,
            Code::L025,
            Code::L026,
            Code::L027,
            Code::L028,
            Code::L029,
            Code::L030,
            Code::L031,
            Code::L032,
        ];
        all.into_iter().find(|c| c.as_str() == s)
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Note,
}

impl Severity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
            Severity::Note => "note",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The lint group a rule belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LintGroup {
    Correct,
    Simplify,
    Style,
    Plugin,
    Perf,
    Docs,
}

impl LintGroup {
    pub const fn as_str(self) -> &'static str {
        match self {
            LintGroup::Correct => "correct",
            LintGroup::Simplify => "simplify",
            LintGroup::Style => "style",
            LintGroup::Plugin => "plugin",
            LintGroup::Perf => "perf",
            LintGroup::Docs => "docs",
        }
    }

    pub fn from_str(s: &str) -> Option<LintGroup> {
        Some(match s {
            "correct" => LintGroup::Correct,
            "simplify" => LintGroup::Simplify,
            "style" => LintGroup::Style,
            "plugin" => LintGroup::Plugin,
            "perf" => LintGroup::Perf,
            "docs" => LintGroup::Docs,
            _ => return None,
        })
    }
}

impl fmt::Display for LintGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a fix is always safe or needs a human check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applicability {
    Machine,
    Maybe,
}

impl Applicability {
    pub const fn as_str(self) -> &'static str {
        match self {
            Applicability::Machine => "machine",
            Applicability::Maybe => "maybe",
        }
    }
}

impl fmt::Display for Applicability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One text replacement in a fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixEdit {
    /// `None` means the diagnostic's own source file.
    pub source: Option<SourceId>,
    pub span: Span,
    pub replacement: String,
}

impl FixEdit {
    pub fn new(span: Span, replacement: impl Into<String>) -> Self {
        Self {
            source: None,
            span,
            replacement: replacement.into(),
        }
    }

    pub fn in_source(source: SourceId, span: Span, replacement: impl Into<String>) -> Self {
        Self {
            source: Some(source),
            span,
            replacement: replacement.into(),
        }
    }
}

/// A list of edits with one applicability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    pub applicability: Applicability,
    pub edits: Vec<FixEdit>,
}

impl Fix {
    pub fn machine(edits: Vec<FixEdit>) -> Self {
        Self {
            applicability: Applicability::Machine,
            edits,
        }
    }

    pub fn maybe(edits: Vec<FixEdit>) -> Self {
        Self {
            applicability: Applicability::Maybe,
            edits,
        }
    }
}

/// A compiler or linter diagnostic with a primary span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: Code,
    pub severity: Severity,
    pub message: String,
    pub source: Option<SourceId>,
    pub span: Option<Span>,
    pub help: Option<String>,
    /// Set for lint diagnostics.
    pub group: Option<LintGroup>,
    /// Present when the rule can repair the diagnostic.
    pub fix: Option<Fix>,
}

impl Diagnostic {
    pub fn error(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: code.default_severity(),
            message: message.into(),
            source: None,
            span: None,
            help: None,
            group: None,
            fix: None,
        }
    }

    pub fn at(code: Code, source: SourceId, span: Span, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: code.default_severity(),
            message: message.into(),
            source: Some(source),
            span: Some(span),
            help: None,
            group: None,
            fix: None,
        }
    }

    pub fn lint(code: Code, source: SourceId, span: Span, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: Severity::Warning,
            message: message.into(),
            source: Some(source),
            span: Some(span),
            help: None,
            group: None,
            fix: None,
        }
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    pub fn with_severity(mut self, severity: Severity) -> Self {
        self.severity = severity;
        self
    }

    pub fn with_group(mut self, group: LintGroup) -> Self {
        self.group = Some(group);
        self
    }

    pub fn with_fix(mut self, fix: Fix) -> Self {
        self.fix = Some(fix);
        self
    }

    /// Human-readable one-diagnostic form.
    pub fn render(&self, sources: &SourceMap) -> String {
        let mut out = String::new();
        if let (Some(source), Some(span)) = (self.source, self.span) {
            if let Some(file) = sources.get(source) {
                let (line, col) = file.line_col(span.start);
                out.push_str(&format!(
                    "{}:{}:{}: {}[{}]: {}",
                    file.name(),
                    line,
                    col,
                    self.severity,
                    self.code,
                    self.message
                ));
            } else {
                out.push_str(&format!(
                    "{}[{}]: {}",
                    self.severity, self.code, self.message
                ));
            }
        } else {
            out.push_str(&format!(
                "{}[{}]: {}",
                self.severity, self.code, self.message
            ));
        }
        if let Some(help) = &self.help {
            out.push_str("\n  help: ");
            out.push_str(help);
        }
        out
    }

    /// One lint diagnostic in the format used by `martite lint`.
    pub fn render_lint(&self, sources: &SourceMap) -> String {
        let severity = match self.severity {
            Severity::Warning => "warn",
            Severity::Info | Severity::Note => "info",
            Severity::Error => "error",
        };
        let mut out = format!("{severity}[{}]: {}", self.code, self.message);
        if let (Some(source), Some(span)) = (self.source, self.span) {
            if let Some(file) = sources.get(source) {
                let (line, col) = file.line_col(span.start);
                let width = line.to_string().len();
                out.push_str(&format!("\n  --> {}:{}:{}", file.name(), line, col));
                let line_text = file.line_text(line);
                out.push_str(&format!("\n{:>width$} |", ""));
                out.push_str(&format!("\n{:>width$} | {}", line, line_text));
                let remaining = line_text
                    .chars()
                    .skip((col.saturating_sub(1)) as usize)
                    .count();
                let label = file.slice(span);
                let mut length = label
                    .split('\n')
                    .next()
                    .map(|s| s.chars().count())
                    .unwrap_or(0);
                length = length.min(remaining).max(1);
                out.push_str(&format!(
                    "\n{:>width$} | {}{}",
                    "",
                    " ".repeat((col.saturating_sub(1)) as usize),
                    "^".repeat(length)
                ));
                let fix_note = self.fix.as_ref().map(|fix| match fix.applicability {
                    Applicability::Machine => " (machine fix)",
                    Applicability::Maybe => " (suggested fix)",
                });
                match (&self.help, fix_note) {
                    (Some(help), Some(note)) => {
                        out.push_str(&format!("\n{:>width$} = help: {help}{note}", ""));
                    }
                    (Some(help), None) => {
                        out.push_str(&format!("\n{:>width$} = help: {help}", ""));
                    }
                    (None, Some(note)) => {
                        out.push_str(&format!("\n{:>width$} = help: fix available{note}", ""));
                    }
                    (None, None) => {}
                }
                return out;
            }
        }
        if let Some(help) = &self.help {
            out.push_str("\n  help: ");
            out.push_str(help);
        }
        out
    }

    /// One JSON object, as required by `--json`.
    pub fn render_json(&self, sources: &SourceMap) -> String {
        let mut out = String::from("{");
        out.push_str(&format!("\"code\":{}", json_str(self.code.as_str())));
        out.push_str(&format!(
            ",\"severity\":{}",
            json_str(self.severity.as_str())
        ));
        out.push_str(&format!(",\"message\":{}", json_str(&self.message)));
        if let Some(source) = self.source {
            if let Some(file) = sources.get(source) {
                out.push_str(&format!(",\"file\":{}", json_str(file.name())));
                if let Some(span) = self.span {
                    let (line, column) = file.line_col(span.start);
                    out.push_str(&format!(
                        ",\"span\":{{\"start\":{},\"end\":{},\"line\":{},\"column\":{}}}",
                        span.start, span.end, line, column
                    ));
                }
            }
        }
        if let Some(help) = &self.help {
            out.push_str(&format!(",\"help\":{}", json_str(help)));
        }
        if let Some(group) = self.group {
            out.push_str(&format!(",\"group\":{}", json_str(group.as_str())));
        }
        if let Some(fix) = &self.fix {
            out.push_str(",\"fix\":[");
            for (index, edit) in fix.edits.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push('{');
                let edit_source = edit.source.or(self.source);
                if let Some(source) = edit_source {
                    if let Some(file) = sources.get(source) {
                        if Some(source) != self.source {
                            out.push_str(&format!("\"file\":{},", json_str(file.name())));
                        }
                        let (line, column) = file.line_col(edit.span.start);
                        out.push_str(&format!(
                            "\"span\":{{\"start\":{},\"end\":{},\"line\":{},\"column\":{}}},",
                            edit.span.start, edit.span.end, line, column
                        ));
                    }
                }
                out.push_str(&format!(
                    "\"replacement\":{},\"applicability\":{}}}",
                    json_str(&edit.replacement),
                    json_str(fix.applicability.as_str())
                ));
            }
            out.push(']');
        }
        out.push('}');
        out
    }
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A diagnostic sink. Output is kept in insertion order and can be sorted by
/// the driver before rendering.
#[derive(Debug, Clone, Default)]
pub struct Emit {
    diagnostics: Vec<Diagnostic>,
}

impl Emit {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn error(&mut self, code: Code, source: SourceId, span: Span, message: impl Into<String>) {
        self.diagnostics
            .push(Diagnostic::at(code, source, span, message));
    }

    pub fn warning(
        &mut self,
        code: Code,
        source: SourceId,
        span: Span,
        message: impl Into<String>,
    ) {
        self.diagnostics
            .push(Diagnostic::lint(code, source, span, message));
    }

    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.diagnostics.push(diagnostic);
    }

    pub fn extend(&mut self, other: impl IntoIterator<Item = Diagnostic>) {
        self.diagnostics.extend(other);
    }

    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }

    pub fn has_diagnostics(&self) -> bool {
        !self.diagnostics.is_empty()
    }

    pub fn as_slice(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub fn into_vec(self) -> Vec<Diagnostic> {
        self.diagnostics
    }

    pub fn sorted(mut self) -> Vec<Diagnostic> {
        self.diagnostics.sort_by_key(|d| {
            (
                d.source.map(|s| s.0).unwrap_or(u32::MAX),
                d.span.map(|s| s.start).unwrap_or(0),
                d.code,
            )
        });
        self.diagnostics
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip() {
        assert_eq!(Code::from_str("E201"), Some(Code::E201));
        assert_eq!(Code::from_str("L021"), Some(Code::L021));
        assert_eq!(Code::from_str("E999"), None);
    }

    #[test]
    fn json_is_valid_escaping() {
        let mut sources = SourceMap::new();
        let id = sources.add("a.mt", "x");
        let d = Diagnostic::at(Code::E201, id, Span::new(0, 1), "bad \"thing\"\n");
        let json = d.render_json(&sources);
        assert!(json.contains("bad \\\"thing\\\"\\n"));
    }
}
