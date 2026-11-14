//! Load pipeline: sources → parse → resolve → check → lower.

use martite_base::{Code, Diagnostic, LoadLimits, Severity, SourceMap, SourceProvider};
use martite_check::{CheckedProgram, check};
use martite_ir::IrProgram;
use martite_resolve::{InterfaceSource, ResolveOptions, resolve};
use martite_syntax::ParseOptions;

pub use martite_ir::LowerOptions;

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
    /// Parser switches. Tooling that needs source parentheses opts in here.
    pub parse: ParseOptions,
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
            parse: options.parse,
        },
        &mut sources,
    );
    let mut diagnostics = resolved.diagnostics;
    let program = match resolved.program {
        Some(resolved) => {
            if diagnostics.iter().any(|d| d.severity == Severity::Error) {
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

/// The result of lowering a checked program, in the same shape as
/// [`LoadedProgram`]: a program on success, or diagnostics on failure.
#[derive(Debug, Clone)]
pub struct LoweredProgram {
    pub program: Option<IrProgram>,
    pub diagnostics: Vec<Diagnostic>,
}

impl LoweredProgram {
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }
}

/// Lower one checked program into self-contained IR.
///
/// Uses [`LowerOptions::default`], which verifies the result only in debug
/// builds. Pass [`LowerOptions`] to [`lower_with`] to force or skip the
/// verifier.
pub fn lower(program: &CheckedProgram, sources: &SourceMap) -> LoweredProgram {
    lower_with(program, sources, LowerOptions::default())
}

/// Lower one checked program with explicit [`LowerOptions`].
pub fn lower_with(
    program: &CheckedProgram,
    sources: &SourceMap,
    options: LowerOptions,
) -> LoweredProgram {
    match martite_ir::lower_with(program, sources, options) {
        Ok(program) => LoweredProgram {
            program: Some(program),
            diagnostics: Vec::new(),
        },
        Err(error) => {
            let mut diagnostic = Diagnostic::error(Code::E508, format!("lowering failed: {error}"));
            diagnostic.source = error.source;
            diagnostic.span = error.span;
            LoweredProgram {
                program: None,
                diagnostics: vec![diagnostic],
            }
        }
    }
}
