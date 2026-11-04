//! Load pipeline: sources → parse → resolve → check.

use martite_base::{Code, Diagnostic, LoadLimits, SourceMap, SourceProvider, Severity};
use martite_check::{check, CheckedProgram};
use martite_resolve::{resolve, InterfaceSource, ResolveOptions};

#[derive(Debug, Clone, Default)]
pub struct LoadOptions {
    /// Entry file relative to the provider root, for example `main.mt`.
    pub entry: String,
    /// Host interface files available to the program.
    pub interfaces: Vec<InterfaceSource>,
    /// Host modules granted by a plugin manifest.
    pub grants: Vec<String>,
    pub limits: LoadLimits,
    /// Language edition; edition 1 is the only supported edition.
    pub edition: String,
}

#[derive(Debug, Clone)]
pub struct LoadedProgram {
    pub program: Option<CheckedProgram>,
    pub sources: SourceMap,
    pub diagnostics: Vec<Diagnostic>,
}

impl LoadedProgram {
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }
}

pub fn load(provider: &dyn SourceProvider, options: &LoadOptions) -> LoadedProgram {
    let mut sources = SourceMap::new();
    if !options.edition.is_empty() && options.edition != "1" {
        let d = Diagnostic::error(
            Code::E508,
            format!("unsupported language edition `{}`", options.edition),
        );
        return LoadedProgram {
            program: None,
            sources,
            diagnostics: vec![d],
        };
    }
    let resolved = resolve(
        provider,
        &ResolveOptions {
            entry: options.entry.clone(),
            interfaces: options.interfaces.clone(),
            grants: options.grants.clone(),
            limits: options.limits,
        },
        &mut sources,
    );
    let mut diagnostics = resolved.diagnostics;
    let program = match resolved.program {
        Some(resolved) => {
            if diagnostics
                .iter()
                .any(|d| d.severity == Severity::Error)
            {
                None
            } else {
                let checked = check(resolved);
                diagnostics.extend(checked.diagnostics);
                checked.program
            }
        }
        None => None,
    };
    diagnostics.sort_by_key(|d| {
        (
            d.source.map(|s| s.0).unwrap_or(u32::MAX),
            d.span.map(|s| s.start).unwrap_or(0),
            d.code,
        )
    });
    LoadedProgram {
        program,
        sources,
        diagnostics,
    }
}
