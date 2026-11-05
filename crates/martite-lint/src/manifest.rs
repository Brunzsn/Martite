//! L020: manifest grants that no source module uses.

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
    let mut searched = 0u32;
    for grant in input.grants {
        if used.contains(grant) {
            continue;
        }
        let Some((span, _quoted)) = find_grant_span(input.text, grant, searched) else {
            continue;
        };
        searched = span.end;
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

fn find_grant_span(text: &str, grant: &str, from: u32) -> Option<(Span, bool)> {
    let start = from.min(text.len() as u32) as usize;
    for (quote, quoted) in [('"', format!("\"{grant}\"")), ('\'', format!("'{grant}'"))] {
        let _ = quote;
        if let Some(offset) = text[start..].find(&quoted) {
            let at = start + offset;
            return Some((Span::new(at as u32, (at + quoted.len()) as u32), true));
        }
    }
    // Fall back to the bare value if the manifest used escaping or a different
    // TOML spelling.
    let offset = text[start..].find(grant)?;
    let at = start + offset;
    Some((Span::new(at as u32, (at + grant.len()) as u32), false))
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
