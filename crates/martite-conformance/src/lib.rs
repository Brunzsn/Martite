//! Shared harness for the Martite conformance suite.

pub mod differential;
pub mod property;
pub mod snapshot;
pub mod snapshot_cases;

use std::collections::HashSet;

use martite_base::{
    Code, Diagnostic, LoadLimits, MemoryProvider, Severity, SourceId, SourceMap, Span,
};
use martite_config::ToolConfig;
use martite_driver::{LoadOptions, LoadedProgram, load};
use martite_ir::{IrProgram, lower, verify};
use martite_lint::{LintInputs, ManifestInput, SettingsResolver, lint};
use martite_resolve::InterfaceSource;
use martite_runtime::{ExecLimits, IrRuntime, MainResult, Trap, Value};
use martite_syntax::ParseOptions;

#[derive(Debug, Clone)]
pub struct Case {
    pub files: Vec<(String, String)>,
    pub entry: String,
    pub interfaces: Vec<InterfaceSource>,
    pub grants: Vec<String>,
    pub limits: LoadLimits,
    pub edition: String,
}

impl Case {
    pub fn single(source: impl Into<String>) -> Self {
        Self {
            files: vec![("main.mt".into(), source.into())],
            entry: "main.mt".into(),
            interfaces: Vec::new(),
            grants: Vec::new(),
            limits: LoadLimits::default(),
            edition: "1".into(),
        }
    }

    pub fn file(mut self, path: impl Into<String>, source: impl Into<String>) -> Self {
        self.files.push((path.into(), source.into()));
        self
    }

    pub fn interface(mut self, path: impl Into<String>, text: impl Into<String>) -> Self {
        self.interfaces.push(InterfaceSource {
            path: path.into(),
            text: text.into(),
        });
        self
    }

    pub fn grant(mut self, path: impl Into<String>) -> Self {
        self.grants.push(path.into());
        self
    }

    pub fn entry(mut self, entry: impl Into<String>) -> Self {
        self.entry = entry.into();
        self
    }

    pub fn limits(mut self, limits: LoadLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn edition(mut self, edition: impl Into<String>) -> Self {
        self.edition = edition.into();
        self
    }

    pub fn load(&self) -> LoadedProgram {
        self.load_with_parse(ParseOptions::default())
    }

    pub fn load_with_parse(&self, parse: ParseOptions) -> LoadedProgram {
        let mut provider = MemoryProvider::new();
        for (path, text) in &self.files {
            provider.insert(path.clone(), text.clone());
        }
        load(
            &provider,
            &LoadOptions {
                entry: self.entry.clone(),
                interfaces: self.interfaces.clone(),
                grants: self.grants.clone(),
                limits: self.limits,
                edition: self.edition.clone(),
                parse,
            },
        )
    }
}

pub fn codes(case: Case) -> Vec<Code> {
    let loaded = case.load();
    loaded.diagnostics.iter().map(|d| d.code).collect()
}

pub fn render(case: Case) -> Vec<String> {
    let loaded = case.load();
    loaded
        .diagnostics
        .iter()
        .map(|d| d.render(&loaded.sources))
        .collect()
}

pub fn assert_code(source: &str, expected: Code) {
    let codes = codes(Case::single(source));
    assert!(
        codes.contains(&expected),
        "expected {expected}, got {codes:?}"
    );
}

/// Assert that a case has no compile diagnostics.
pub fn assert_clean(case: &Case) {
    let loaded = case.load();
    assert!(
        loaded.diagnostics.is_empty(),
        "expected a clean load, got:\n{}",
        loaded
            .diagnostics
            .iter()
            .map(|d| d.render(&loaded.sources))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Assert one diagnostic with an optional primary-span source slice, message
/// substring, and help substring.
pub fn assert_diagnostic(
    case: &Case,
    code: Code,
    span_text: Option<&str>,
    message_contains: Option<&str>,
    help_contains: Option<&str>,
) {
    let loaded = case.load();
    let matching: Vec<&Diagnostic> = loaded
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == code)
        .collect();
    assert!(
        !matching.is_empty(),
        "expected {code}, got {:?}: {}",
        loaded
            .diagnostics
            .iter()
            .map(|d| d.code)
            .collect::<Vec<_>>(),
        loaded
            .diagnostics
            .iter()
            .map(|d| d.render(&loaded.sources))
            .collect::<Vec<_>>()
            .join("\n")
    );

    let mut failures = Vec::new();
    for diagnostic in &matching {
        if let Some(expected) = span_text {
            let actual = diagnostic
                .source
                .and_then(|source| loaded.sources.get(source))
                .zip(diagnostic.span)
                .map(|(file, span)| file.slice(span))
                .unwrap_or("");
            if actual != expected {
                failures.push(format!(
                    "span mismatch: expected {expected:?}, got {actual:?} ({})",
                    diagnostic.render(&loaded.sources)
                ));
                continue;
            }
        }
        if let Some(expected) = message_contains
            && !diagnostic.message.contains(expected)
        {
            failures.push(format!(
                "message mismatch: expected substring {expected:?}, got {:?}",
                diagnostic.message
            ));
            continue;
        }
        if let Some(expected) = help_contains {
            match &diagnostic.help {
                Some(help) if help.contains(expected) => {}
                other => failures.push(format!(
                    "help mismatch: expected {expected:?}, got {other:?}"
                )),
            }
        }
        if failures.is_empty() {
            return;
        }
    }
    panic!("{code} assertions failed:\n{}", failures.join("\n"));
}

pub struct LintRun {
    pub sources: SourceMap,
    pub diagnostics: Vec<Diagnostic>,
}

impl LintRun {
    pub fn codes(&self) -> Vec<Code> {
        self.diagnostics.iter().map(|d| d.code).collect()
    }

    pub fn find(&self, code: Code) -> Option<&Diagnostic> {
        self.diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == code)
    }

    pub fn span_text(&self, diagnostic: &Diagnostic) -> &str {
        diagnostic
            .source
            .and_then(|source| self.sources.get(source))
            .zip(diagnostic.span)
            .map(|(file, span)| file.slice(span))
            .unwrap_or("")
    }

    pub fn assert_has(&self, code: Code, span_text: Option<&str>, message_contains: Option<&str>) {
        let Some(diagnostic) = self.find(code) else {
            panic!(
                "expected {code}, got {:?}:\n{}",
                self.codes(),
                self.diagnostics
                    .iter()
                    .map(|d| d.render(&self.sources))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        };
        if let Some(expected) = span_text {
            let actual = self.span_text(diagnostic);
            assert_eq!(actual, expected, "primary span for {code}");
        }
        if let Some(expected) = message_contains {
            assert!(
                diagnostic.message.contains(expected),
                "message for {code}: {:?} does not contain {expected:?}",
                diagnostic.message
            );
        }
    }

    pub fn assert_not(&self, code: Code) {
        assert!(
            self.find(code).is_none(),
            "unexpected {code}:\n{}",
            self.diagnostics
                .iter()
                .map(|d| d.render(&self.sources))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

/// Load a case and lint it. The case must type-check.
pub fn lint_case(
    case: &Case,
    config: &ToolConfig,
    selected: Option<HashSet<Code>>,
    manifest_text: Option<&str>,
) -> LintRun {
    let loaded = case.load_with_parse(ParseOptions::TOOLING);
    let errors: Vec<&Diagnostic> = loaded
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "lint fixture must type-check:\n{}",
        errors
            .iter()
            .map(|d| d.render(&loaded.sources))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let program = loaded.program.as_ref().expect("checked program");
    let resolver = SettingsResolver::new(config, selected).expect("valid settings");
    let mut sources = loaded.sources.clone();
    let manifest_source = manifest_text.map(|text| sources.add("plugin.toml", text));
    let inputs = match (manifest_source, manifest_text) {
        (Some(source), Some(text)) => LintInputs {
            manifest: Some(ManifestInput {
                source,
                text,
                grants: &case.grants,
            }),
        },
        _ => LintInputs::default(),
    };
    let output = lint(program, &sources, &resolver, "", &inputs);
    LintRun {
        sources,
        diagnostics: output.diagnostics,
    }
}

pub fn run(source: &str) -> Vec<String> {
    run_case(Case::single(source)).expect("program should run")
}

pub fn run_case(case: Case) -> Result<Vec<String>, Trap> {
    run_case_with_limits(&case, ExecLimits::default()).map(|main| main.log)
}

pub fn run_case_with_limits(case: &Case, limits: ExecLimits) -> Result<MainResult, Trap> {
    let loaded = case.load();
    if loaded
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error)
    {
        let sources: &SourceMap = &loaded.sources;
        panic!(
            "unexpected diagnostics:\n{}",
            loaded
                .diagnostics
                .iter()
                .map(|d| d.render(sources))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    let program = loaded.program.expect("program");
    let ir = lower(&program, &loaded.sources).expect("fixture lowers");
    verify(&ir).expect("fixture verifies");
    let mut runtime = IrRuntime::new(&ir, limits);
    let main = runtime.run_main()?;
    if let Value::Result(Err(error)) = &main.value {
        return Err(Trap::Panic {
            message: martite_runtime::show(error, &ir.types, true),
            source: None,
            span: None,
            trace: Vec::new(),
        });
    }
    Ok(main)
}

pub fn trap(source: &str) -> Trap {
    run_case(Case::single(source)).expect_err("program should trap")
}

/// Upper bounds used when binary-searching a fixture's thresholds.
#[derive(Debug, Clone, Copy)]
pub struct ProbeCaps {
    pub fuel: u64,
    pub memory: u64,
    pub depth: u32,
}

impl Default for ProbeCaps {
    fn default() -> Self {
        Self {
            fuel: 1_000_000,
            memory: 8 << 20,
            depth: 200,
        }
    }
}

/// Lower and verify a fixture once for direct evaluator tests.
pub fn lower_case_ir(case: &Case) -> IrProgram {
    let loaded = case.load();
    assert!(
        !loaded
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error),
        "program must load cleanly:\n{}",
        loaded
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.render(&loaded.sources))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let program = loaded.program.expect("program");
    let ir = lower(&program, &loaded.sources).expect("fixture lowers");
    verify(&ir).expect("fixture verifies");
    ir
}

fn run_with_limits(program: &IrProgram, limits: ExecLimits) -> Result<(), Trap> {
    let mut runtime = IrRuntime::new(program, limits);
    runtime.run_main().map(|_| ())
}

/// Smallest fuel budget that lets the program finish. The interpreter traps
/// when a charge drives the remaining budget to zero, so this is one more
/// than the total specified cost.
pub fn min_fuel(case: &Case) -> u64 {
    min_fuel_with(case, ProbeCaps::default())
}

pub fn min_fuel_with(case: &Case, caps: ProbeCaps) -> u64 {
    let program = lower_case_ir(case);
    let mut low = 1u64;
    let mut high = caps.fuel.max(1);
    assert!(
        run_with_limits(
            &program,
            ExecLimits {
                fuel: high,
                memory: caps.memory,
                depth: caps.depth,
                deadline: None,
            },
        )
        .is_ok(),
        "fixture needs more than the fuel probe cap"
    );
    while low < high {
        let mid = low + (high - low) / 2;
        match run_with_limits(
            &program,
            ExecLimits {
                fuel: mid,
                memory: caps.memory,
                depth: caps.depth,
                deadline: None,
            },
        ) {
            Ok(()) => high = mid,
            Err(Trap::OutOfFuel) => low = mid + 1,
            Err(trap) => panic!("unexpected trap while probing fuel: {trap:?}"),
        }
    }
    low
}

/// Smallest memory budget that lets the program finish, i.e. its high-water
/// mark under the specified accounting.
pub fn min_memory(case: &Case) -> u64 {
    min_memory_with(case, ProbeCaps::default())
}

pub fn min_memory_with(case: &Case, caps: ProbeCaps) -> u64 {
    let program = lower_case_ir(case);
    let mut low = 0u64;
    let mut high = caps.memory;
    assert!(
        run_with_limits(
            &program,
            ExecLimits {
                fuel: caps.fuel,
                memory: high,
                depth: caps.depth,
                deadline: None,
            },
        )
        .is_ok(),
        "fixture needs more than the memory probe cap"
    );
    while low < high {
        let mid = low + (high - low) / 2;
        match run_with_limits(
            &program,
            ExecLimits {
                fuel: caps.fuel,
                memory: mid,
                depth: caps.depth,
                deadline: None,
            },
        ) {
            Ok(()) => high = mid,
            Err(Trap::OutOfMemory) => low = mid + 1,
            Err(trap) => panic!("unexpected trap while probing memory: {trap:?}"),
        }
    }
    low
}

/// Smallest call-depth budget that lets the program finish.
pub fn min_depth(case: &Case) -> u32 {
    min_depth_with(case, ProbeCaps::default())
}

pub fn min_depth_with(case: &Case, caps: ProbeCaps) -> u32 {
    let program = lower_case_ir(case);
    let mut low = 0u32;
    let mut high = caps.depth;
    assert!(
        run_with_limits(
            &program,
            ExecLimits {
                fuel: caps.fuel,
                memory: caps.memory,
                depth: high,
                deadline: None,
            },
        )
        .is_ok(),
        "fixture needs more than the depth probe cap"
    );
    while low < high {
        let mid = low + (high - low) / 2;
        match run_with_limits(
            &program,
            ExecLimits {
                fuel: caps.fuel,
                memory: caps.memory,
                depth: mid,
                deadline: None,
            },
        ) {
            Ok(()) => high = mid,
            Err(Trap::DepthExceeded) => low = mid + 1,
            Err(trap) => panic!("unexpected trap while probing depth: {trap:?}"),
        }
    }
    low
}

pub fn source_slice(sources: &SourceMap, source: SourceId, span: Span) -> &str {
    sources
        .get(source)
        .map(|file| file.slice(span))
        .unwrap_or("")
}
