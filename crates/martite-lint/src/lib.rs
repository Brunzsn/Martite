//! Linter for Martite.
//!
//! The linter is deliberately split by analysis tier. Syntax-tier rules can run
//! on a parsed file alone; higher tiers need the checked program and are added
//! to [`lint`] as they are implemented.

mod analysis;
mod checked;
mod manifest;
mod registry;
mod settings;
mod suppress;
mod syntax;
mod trivia;

use martite_base::{Code, Diagnostic, SourceId, SourceMap};
use martite_check::CheckedProgram;
use martite_syntax::Program;

pub use registry::{
    Level, RETIRED, RULES, RuleMeta, Tier, explain_text, is_known, is_retired, list_rules_json,
    list_rules_text, parse_selection, rule,
};
pub use settings::{
    ConfigError, Options, ResolvedSettings, SettingsResolver, Thresholds, glob_match,
    normalize_path,
};

/// Extra inputs the higher-tier rules need.
#[derive(Debug, Clone, Copy, Default)]
pub struct LintInputs<'a> {
    pub manifest: Option<ManifestInput<'a>>,
}

#[derive(Debug, Clone, Copy)]
pub struct ManifestInput<'a> {
    pub source: SourceId,
    pub text: &'a str,
    pub grants: &'a [String],
}

#[derive(Debug, Clone, Default)]
pub struct LintOutput {
    pub diagnostics: Vec<Diagnostic>,
}

/// Lint a fully loaded program. This currently runs the syntax tier and the
/// suppression pass; higher tiers are added on top of the same context.
pub fn lint(
    program: &CheckedProgram,
    sources: &SourceMap,
    resolver: &SettingsResolver,
    path_prefix: &str,
    inputs: &LintInputs<'_>,
) -> LintOutput {
    let prefix = normalize_path(path_prefix);
    let mut diagnostics = Vec::new();
    assert!(
        program
            .resolved
            .modules
            .iter()
            .all(|module| module.ast.records_parentheses()),
        "lint requires programs loaded with LoadOptions::parse = martite_syntax::ParseOptions::TOOLING"
    );
    let analysis = analysis::Analysis::new(program, sources);
    for module in &program.resolved.modules {
        if module.is_interface {
            continue;
        }
        let Some(file) = sources.get(module.source) else {
            continue;
        };
        let relative = if prefix.is_empty() {
            normalize_path(file.name())
        } else {
            normalize_path(&format!("{prefix}/{}", file.name()))
        };
        let settings = resolver.resolve(&relative);
        let mut file_diagnostics = syntax::run(module.source, file.text(), &module.ast, &settings);
        if let Some(module_ref) = analysis.module(module.id) {
            file_diagnostics.extend(checked::run(program, &analysis, module_ref, &settings));
        }
        file_diagnostics = suppress::apply(
            module.source,
            file.text(),
            &module.ast,
            &settings,
            file_diagnostics,
            true,
        );
        diagnostics.extend(file_diagnostics);
    }
    if let Some(manifest) = inputs.manifest {
        diagnostics.extend(manifest::run(manifest, program, resolver, &prefix));
    }
    sort_diagnostics(&mut diagnostics, sources);
    LintOutput { diagnostics }
}

/// Parse-only linting for editors. Runs syntax-tier rules on one file and never
/// reads interfaces, manifests, or other modules.
pub fn lint_syntax_only(
    source: SourceId,
    text: &str,
    program: &Program,
    settings: &ResolvedSettings,
) -> Vec<Diagnostic> {
    assert!(
        program.records_parentheses(),
        "lint_syntax_only requires a program parsed with martite_syntax::ParseOptions::TOOLING"
    );
    let diagnostics = syntax::run(source, text, program, settings);
    suppress::apply(source, text, program, settings, diagnostics, false)
}

pub fn sort_diagnostics(diagnostics: &mut [Diagnostic], sources: &SourceMap) {
    diagnostics.sort_by(|left, right| {
        let left_file = left
            .source
            .and_then(|source| sources.get(source))
            .map(|file| file.name())
            .unwrap_or("");
        let right_file = right
            .source
            .and_then(|source| sources.get(source))
            .map(|file| file.name())
            .unwrap_or("");
        left_file
            .cmp(right_file)
            .then_with(|| {
                left.span
                    .map(|span| span.start)
                    .unwrap_or(u32::MAX)
                    .cmp(&right.span.map(|span| span.start).unwrap_or(u32::MAX))
            })
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.message.cmp(&right.message))
    });
}

/// True when a diagnostic should make `martite lint` exit with 1.
pub fn has_failing_diagnostics(diagnostics: &[Diagnostic], deny_warnings: bool) -> bool {
    diagnostics.iter().any(|diagnostic| {
        diagnostic.severity == martite_base::Severity::Error
            || (deny_warnings && diagnostic.severity == martite_base::Severity::Warning)
    })
}

/// Whether a code participates in the selected rule set.
pub fn selected(settings: &ResolvedSettings, code: Code) -> bool {
    settings.selected(code)
}
