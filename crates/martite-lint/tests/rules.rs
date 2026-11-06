use martite_base::{Code, MemoryProvider, SourceId};
use martite_config::ToolConfig;
use martite_driver::{LoadOptions, load};
use martite_lint::{LintInputs, ManifestInput, SettingsResolver, lint, lint_syntax_only};
use martite_resolve::InterfaceSource;
use martite_syntax::{ParseOptions, parse_source_with};

fn resolver(preset: &str) -> SettingsResolver {
    let config = ToolConfig {
        lint_preset: Some(preset.into()),
        ..ToolConfig::default()
    };
    SettingsResolver::new(&config, None).expect("valid settings")
}

fn codes(diagnostics: &[martite_base::Diagnostic]) -> Vec<Code> {
    diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code)
        .collect()
}

fn lint_script(source: &str, preset: &str) -> Vec<Code> {
    lint_script_with(source, &resolver(preset))
}

fn lint_script_with(source: &str, settings: &SettingsResolver) -> Vec<Code> {
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", source);
    let loaded = load(
        &provider,
        &LoadOptions {
            entry: "main.mt".into(),
            interfaces: Vec::new(),
            grants: Vec::new(),
            limits: Default::default(),
            edition: "1".into(),
            parse: ParseOptions::TOOLING,
        },
    );
    assert!(
        !loaded
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == martite_base::Severity::Error),
        "fixture must type-check: {:?}",
        codes(&loaded.diagnostics)
    );
    let program = loaded.program.as_ref().expect("checked program");
    let output = lint(
        program,
        &loaded.sources,
        settings,
        "",
        &LintInputs::default(),
    );
    codes(&output.diagnostics)
}

#[test]
fn syntax_tier_reports_basic_rules() {
    let source = r#"
fn main() {
    if true {}
    let first = true == false
    let second = if first { true } else { false }
    if second and first or false {
        print("mixed")
    }
    /// stray
    let value = 1
    print(value)
}
"#;
    let parsed = parse_source_with(source, Default::default(), ParseOptions::TOOLING);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let settings = resolver("recommended").resolve("main.mt");
    let diagnostics = lint_syntax_only(SourceId(0), source, &parsed.program, &settings);
    let found = codes(&diagnostics);
    for expected in [Code::L005, Code::L011, Code::L016, Code::L024, Code::L032] {
        assert!(found.contains(&expected), "missing {expected}: {found:?}");
    }
}

#[test]
fn typed_and_resolved_rules_report_basic_rules() {
    let source = r#"
fn main() {
    var total = 0
    total = total + 1
    let same = 1
    if same == same {
        print("same")
    }
    let ratio = 1.0
    if ratio == 0.5 {
        print("float")
    }
    let zero = 10 / 0
    print(zero)
    let _ = fail()
    return
    print("never")
}

fn fail() -> Result[str, str] {
    Err("x")
}
"#;
    let found = lint_script(source, "recommended");
    for expected in [Code::L006, Code::L007, Code::L009, Code::L010, Code::L004] {
        assert!(found.contains(&expected), "missing {expected}: {found:?}");
    }
}

fn plugin_case(source: &str, grants: &[&str], interfaces: &[(&str, &str)]) -> Vec<Code> {
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", source);
    let interfaces: Vec<InterfaceSource> = interfaces
        .iter()
        .map(|(path, text)| InterfaceSource {
            path: (*path).into(),
            text: (*text).into(),
        })
        .collect();
    let loaded = load(
        &provider,
        &LoadOptions {
            entry: "main.mt".into(),
            interfaces,
            grants: grants.iter().map(|grant| (*grant).into()).collect(),
            limits: Default::default(),
            edition: "1".into(),
            parse: ParseOptions::TOOLING,
        },
    );
    assert!(
        !loaded
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == martite_base::Severity::Error),
        "fixture must type-check: {:?}",
        codes(&loaded.diagnostics)
    );
    let program = loaded.program.as_ref().expect("checked program");
    let settings = resolver("strict");
    let output = lint(
        program,
        &loaded.sources,
        &settings,
        "",
        &LintInputs::default(),
    );
    codes(&output.diagnostics)
}

#[test]
fn plugin_flow_rules_fire() {
    let interface = r#"
struct Doc
fn open() -> Doc
fn maybe() -> str?
on tick()
on add(item: str) -> Result[(), str]
on check()
"#;
    let source = r#"
use app.editor
use app.editor.Doc

struct State { doc: Doc }

on init() -> State {
    State { doc: editor.open() }
}

on tick(mut self) {
    while true {
        self.doc = editor.open()
    }
}

on add(mut self, item: str) -> Result[(), str] {
    self.doc = editor.open()
    if item == "" {
        return Err("empty")
    }
    Ok(())
}

on check(mut self) {
    let value = editor.maybe().expect("missing")
    print(value)
}
"#;
    let found = plugin_case(source, &["app.editor"], &[("app.editor", interface)]);
    for expected in [Code::L021, Code::L019, Code::L027, Code::L008, Code::L031] {
        assert!(found.contains(&expected), "missing {expected}: {found:?}");
    }
}

#[test]
fn l020_reports_unused_grant() {
    let interface = "struct Doc\non check()\n";
    let unused = "struct Other\n";
    let source = "use app.editor\non check() {}\n";
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", source);
    let interfaces = vec![
        InterfaceSource {
            path: "app.editor".into(),
            text: interface.into(),
        },
        InterfaceSource {
            path: "app.unused".into(),
            text: unused.into(),
        },
    ];
    let loaded = load(
        &provider,
        &LoadOptions {
            entry: "main.mt".into(),
            interfaces,
            grants: vec!["app.editor".into(), "app.unused".into()],
            limits: Default::default(),
            edition: "1".into(),
            parse: ParseOptions::TOOLING,
        },
    );
    assert!(!loaded.has_errors(), "{:?}", codes(&loaded.diagnostics));
    let mut sources = loaded.sources.clone();
    let manifest_source = sources.add(
        "plugin.toml",
        "[plugin]\nname = \"x\"\nversion = \"1\"\nlang = \"1\"\napi = \"1\"\nentry = \"main.mt\"\n\n[permissions]\ngrant = [\"app.editor\", \"app.unused\"]\n",
    );
    let manifest = "[plugin]\nname = \"x\"\nversion = \"1\"\nlang = \"1\"\napi = \"1\"\nentry = \"main.mt\"\n\n[permissions]\ngrant = [\"app.editor\", \"app.unused\"]\n";
    let inputs = LintInputs {
        manifest: Some(ManifestInput {
            source: manifest_source,
            text: manifest,
            grants: &["app.editor".into(), "app.unused".into()],
        }),
    };
    let settings = resolver("recommended");
    let output = lint(
        loaded.program.as_ref().unwrap(),
        &sources,
        &settings,
        "",
        &inputs,
    );
    assert!(codes(&output.diagnostics).contains(&Code::L020));
}

#[test]
fn l022_reports_unknown_and_unused_suppressions() {
    let source = "// allow(L999)\nfn main() {\n    if true {}\n}\n";
    let found = lint_script(source, "recommended");
    assert!(found.contains(&Code::L022), "{found:?}");
}

#[test]
fn suppression_marks_a_used_id_and_skips_its_diagnostic() {
    let source = "fn main() {\n    // allow(L002)\n    var value = 1\n    print(value)\n}\n";
    let found = lint_script(source, "recommended");
    assert!(!found.contains(&Code::L002), "{found:?}");
    assert!(!found.contains(&Code::L022), "{found:?}");
}

#[test]
fn l022_ignores_ids_excluded_by_rule_selection() {
    let source = "fn main() {\n    // allow(L002)\n    var value = 1\n    print(value)\n}\n";
    let config = ToolConfig::default();
    let selected = Some(std::collections::HashSet::from([Code::L005, Code::L022]));
    let settings = SettingsResolver::new(&config, selected).unwrap();
    let found = lint_script_with(source, &settings);
    assert!(!found.contains(&Code::L022), "{found:?}");
}

#[test]
fn syntax_metrics_and_docs_fire() {
    let source = r#"
pub fn work(a: int, b: int, c: int, d: int, e: int, f: int) -> int {
    if a > 0 {
        if b > 0 {
            return 1
        }
    }
    2
}

fn main() {
    print("{work(1, 2, 3, 4, 5, 6)}")
}
"#;
    let parsed = parse_source_with(source, Default::default(), ParseOptions::TOOLING);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let config = ToolConfig {
        lint_preset: Some("strict".into()),
        lint_thresholds: [
            ("max_fn_lines".into(), 2),
            ("max_params".into(), 2),
            ("max_nesting".into(), 1),
            ("max_branches".into(), 2),
        ]
        .into_iter()
        .collect(),
        ..ToolConfig::default()
    };
    let settings = SettingsResolver::new(&config, None)
        .unwrap()
        .resolve("main.mt");
    let found = codes(&lint_syntax_only(
        SourceId(0),
        source,
        &parsed.program,
        &settings,
    ));
    assert!(found.contains(&Code::L017), "{found:?}");
    assert!(found.contains(&Code::L018), "{found:?}");
}

#[test]
fn l013_else_and_l014_bool_match_fire() {
    let source = r#"
fn branch(flag: bool) {
    if flag {
        return
    } else {
        print("else")
    }
    print("after")
}

fn choose(flag: bool) -> int {
    match flag {
        true => 1,
        false => 0,
    }
}

fn main() {
    branch(true)
    print("{choose(true)}")
}
"#;
    let found = lint_script(source, "recommended");
    assert!(found.contains(&Code::L013), "{found:?}");
    assert!(found.contains(&Code::L014), "{found:?}");
}

#[test]
fn l026_reports_panicking_operations_in_handlers() {
    let interface = "on add(xs: list[int]) -> Result[(), str]\n";
    let source = r#"
struct State { n: int }

on init() -> State {
    State { n: 0 }
}

on add(mut self, xs: list[int]) -> Result[(), str] {
    let first = xs[0]
    self.n += 1
    print(first)
    Ok(())
}
"#;
    let found = plugin_case(source, &["app.api"], &[("app.api", interface)]);
    assert!(found.contains(&Code::L026), "{found:?}");
}

#[test]
fn l027_uses_helper_summaries() {
    let interface = "on add() -> Result[(), str]\n";
    let source = r#"
use app.api

struct State { n: int }

on init() -> State {
    State { n: 0 }
}

on add(mut self) -> Result[(), str] {
    bump(mut self)?
    Ok(())
}

fn bump(mut state: State) -> Result[(), str] {
    state.n += 1
    Err("x")
}
"#;
    let found = plugin_case(source, &["app.api"], &[("app.api", interface)]);
    let count = found.iter().filter(|code| **code == Code::L027).count();
    assert!(count >= 2, "expected helper and caller, got {found:?}");
}
