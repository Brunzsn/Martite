//! Shared harness for the Martite conformance suite.

use martite_base::{Code, LoadLimits, MemoryProvider, Severity, SourceMap};
use martite_driver::{LoadOptions, LoadedProgram, load};
use martite_resolve::InterfaceSource;
use martite_runtime::{ExecLimits, Runtime, Trap, Value};

pub struct Case {
    pub files: Vec<(String, String)>,
    pub entry: String,
    pub interfaces: Vec<InterfaceSource>,
    pub grants: Vec<String>,
    pub limits: LoadLimits,
}

impl Case {
    pub fn single(source: impl Into<String>) -> Self {
        Self {
            files: vec![("main.mt".into(), source.into())],
            entry: "main.mt".into(),
            interfaces: Vec::new(),
            grants: Vec::new(),
            limits: LoadLimits::default(),
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

    pub fn load(&self) -> LoadedProgram {
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
                edition: "1".into(),
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

pub fn run(source: &str) -> Vec<String> {
    run_case(Case::single(source)).expect("program should run")
}

pub fn run_case(case: Case) -> Result<Vec<String>, Trap> {
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
    let mut runtime = Runtime::new(&program, ExecLimits::default());
    let main = runtime.run_main()?;
    if let Value::Result(Err(error)) = &main.value {
        return Err(Trap::Panic {
            message: martite_runtime::show(error, &program.types, true),
            source: None,
            span: None,
            trace: Vec::new(),
        });
    }
    Ok(main.log)
}

pub fn trap(source: &str) -> Trap {
    run_case(Case::single(source)).expect_err("program should trap")
}
