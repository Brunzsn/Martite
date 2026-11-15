//! L020: manifest grants that no source module uses.

use std::collections::HashMap;

use martite_base::{Applicability, Code, Diagnostic, Fix, FixEdit, LintGroup, Span};
use martite_check::CheckedProgram;
use martite_resolve::ModuleTargetUse;

use crate::registry;
use crate::settings::SettingsResolver;
use crate::{ManifestInput, normalize_path};

pub fn run(
    input: ManifestInput<'_>,
    program: &CheckedProgram,
    resolver: &SettingsResolver,
    path_prefix: &str,
) -> Vec<Diagnostic> {
    let relative = if path_prefix.is_empty() {
        "plugin.toml".to_string()
    } else {
        normalize_path(&format!("{path_prefix}/plugin.toml"))
    };
    let settings = resolver.resolve(&relative);
    let Some(severity) = settings.level(Code::L020).severity() else {
        return Vec::new();
    };
    let mut used = std::collections::HashSet::new();
    for module in &program.resolved.modules {
        if module.is_interface {
            continue;
        }
        for import in &module.uses {
            let path = import.path.join(".");
            match &import.target {
                ModuleTargetUse::Module(target) => {
                    if let Some(target_module) = module_path(program, target) {
                        used.insert(target_module);
                    }
                }
                ModuleTargetUse::Item(def) => {
                    let info = program.def(*def);
                    let target = program.resolved.module(info.module);
                    used.insert(target.path.clone());
                }
                ModuleTargetUse::BuiltinItem(_) => {}
            }
            if !path.is_empty() {
                used.insert(path);
            }
        }
    }

    let mut diagnostics = Vec::new();
    let spans = grant_spans(input.text);
    for grant in input.grants {
        if used.contains(grant) {
            continue;
        }
        let Some(span) = spans.get(grant).copied() else {
            continue;
        };
        let edit_span = grant_removal_span(input.text, span);
        let fix = Fix {
            applicability: Applicability::Maybe,
            edits: vec![FixEdit::in_source(input.source, edit_span, "")],
        };
        let mut diagnostic = Diagnostic::lint(
            Code::L020,
            input.source,
            span,
            format!("grant `{grant}` is never used"),
        )
        .with_severity(severity)
        .with_group(LintGroup::Plugin)
        .with_help("remove the grant or import the module that needs it")
        .with_fix(fix);
        diagnostic.group = Some(
            registry::rule(Code::L020)
                .map(|r| r.group)
                .unwrap_or(LintGroup::Plugin),
        );
        diagnostics.push(diagnostic);
    }
    diagnostics
}

fn module_path(program: &CheckedProgram, target: &martite_check::ModuleTarget) -> Option<String> {
    match target {
        martite_check::ModuleTarget::Source(id) | martite_check::ModuleTarget::Interface(id) => {
            Some(program.resolved.module(*id).path.clone())
        }
        martite_check::ModuleTarget::Builtin(name) => Some((*name).to_string()),
    }
}

/// Map each decoded grant value to the exact span of its TOML string literal.
fn grant_spans(text: &str) -> HashMap<String, Span> {
    let mut spans = HashMap::new();
    let Ok(document) = toml_edit::Document::parse(text.to_owned()) else {
        return spans;
    };
    let Some(grants) = document
        .get("permissions")
        .and_then(toml_edit::Item::as_table_like)
        .and_then(|permissions| permissions.get("grant"))
        .and_then(toml_edit::Item::as_array)
    else {
        return spans;
    };
    for value in grants.iter() {
        let Some((grant, span)) = value.as_str().zip(value.span()) else {
            continue;
        };
        if span.start > span.end || span.end > text.len() {
            continue;
        }
        spans
            .entry(grant.to_string())
            .or_insert(Span::new(span.start as u32, span.end as u32));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_spans_follow_decoded_toml_values() {
        let text = "[permissions]\ngrant = [\"app.editor\", \"a\\u0070p\", 'plain']\n";
        let spans = grant_spans(text);
        let slice = |grant: &str| {
            let span = spans[grant];
            &text[span.start as usize..span.end as usize]
        };
        assert_eq!(slice("app.editor"), "\"app.editor\"");
        assert_eq!(slice("app"), "\"a\\u0070p\"");
        assert_eq!(slice("plain"), "'plain'");
    }
}

/// Expand a quoted grant string to include one adjacent comma, so the array
/// stays valid after the edit.
fn grant_removal_span(text: &str, span: Span) -> Span {
    let bytes = text.as_bytes();
    let mut end = span.end as usize;
    let mut after = end;
    while after < bytes.len() && bytes[after].is_ascii_whitespace() {
        after += 1;
    }
    if after < bytes.len() && bytes[after] == b',' {
        end = after + 1;
        while end < bytes.len() && bytes[end].is_ascii_whitespace() && bytes[end] != b'\n' {
            end += 1;
        }
        return Span::new(span.start, end as u32);
    }
    let mut start = span.start as usize;
    let mut before = start;
    while before > 0 && bytes[before - 1].is_ascii_whitespace() && bytes[before - 1] != b'\n' {
        before -= 1;
    }
    if before > 0 && bytes[before - 1] == b',' {
        start = before - 1;
    }
    Span::new(start as u32, span.end)
}
