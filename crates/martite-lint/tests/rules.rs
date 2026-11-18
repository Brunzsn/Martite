use martite_base::{Code, Fix, MemoryProvider, SourceId};
use martite_config::ToolConfig;
use martite_driver::{LoadOptions, load};
use martite_lint::{LintInputs, ManifestInput, SettingsResolver, lint, lint_syntax_only};
use martite_resolve::InterfaceSource;
use martite_syntax::parse_source;

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
    codes(&lint_script_diagnostics_with(source, settings))
}

fn apply_fix(source: &str, fix: &Fix) -> String {
    apply_fixes(source, std::iter::once(fix))
}

fn apply_fixes<'a>(source: &str, fixes: impl IntoIterator<Item = &'a Fix>) -> String {
    let mut edits: Vec<_> = fixes.into_iter().flat_map(|fix| fix.edits.iter()).collect();
    edits.sort_by_key(|edit| std::cmp::Reverse((edit.span.start, edit.span.end)));
    let mut text = source.to_string();
    for edit in edits {
        text.replace_range(
            edit.span.start as usize..edit.span.end as usize,
            &edit.replacement,
        );
    }
    text
}

fn fix_for(diagnostics: &[martite_base::Diagnostic], code: Code) -> &Fix {
    diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == code)
        .and_then(|diagnostic| diagnostic.fix.as_ref())
        .unwrap_or_else(|| panic!("expected a {code} fix in {:?}", codes(diagnostics)))
}

fn lint_script_diagnostics_with(
    source: &str,
    settings: &SettingsResolver,
) -> Vec<martite_base::Diagnostic> {
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
    lint(
        program,
        &loaded.sources,
        settings,
        "",
        &LintInputs::default(),
    )
    .diagnostics
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
    let parsed = parse_source(source, Default::default());
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
fn l024_uses_the_parsed_operator_span() {
    let source = "fn check(a: bool, b: bool, c: bool) -> bool {\n    a and b // or\n    or c\n}\n\ntest \"mixed\" {\n    assert(check(true, true, false), \"yes\")\n}\n";
    let parsed = parse_source(source, Default::default());
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let settings = resolver("recommended").resolve("main.mt");
    let diagnostics = lint_syntax_only(SourceId(0), source, &parsed.program, &settings);
    let l024 = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == Code::L024)
        .expect("expected L024");
    let span = l024.span.expect("L024 span");
    assert_eq!(&source[span.start as usize..span.end as usize], "or");
    let comment = source.find("// or").expect("comment");
    assert!(
        span.start as usize > comment,
        "matched the comment: {span:?}"
    );
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
fn l022_reports_only_the_unused_id_in_a_mixed_suppression() {
    let source = "fn main() {\n    // allow(L002, L005)\n    var value = 1\n    print(value)\n}\n";
    let diagnostics = lint_script_diagnostics_with(source, &resolver("recommended"));
    let found = codes(&diagnostics);
    assert!(!found.contains(&Code::L002), "{found:?}");
    let l022 = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == Code::L022)
        .expect("expected L022");
    assert!(
        l022.message.contains("`L005` suppresses nothing here"),
        "{}",
        l022.message
    );
    assert!(!l022.message.contains('\0'), "{}", l022.message);
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
    let parsed = parse_source(source, Default::default());
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

#[test]
fn l003_keeps_catch_all_needed_by_nested_or_pattern() {
    let source = r#"
enum E { A(int), B }

fn score(e: E) -> int {
    match e {
        E.A(0 | 1) => 1,
        _ => 2,
    }
}

fn main() {
    print("{score(E.A(5))}")
}
"#;
    let found = lint_script(source, "recommended");
    assert!(!found.contains(&Code::L003), "{found:?}");
}

#[test]
fn l011_not_spans_cover_the_negation_and_fixes_are_idempotent() {
    let source = "fn first(a: bool, b: bool) -> bool {\n    not a == b\n}\n\nfn second(a: bool, b: bool) -> bool {\n    not (a == b)\n}\n\nfn third(a: bool, b: bool, c: bool) -> bool {\n    not ((a or b) == c)\n}\n\nfn main() {\n    print(\"{first(true, false)}{second(true, false)}{third(false, false, false)}\")\n}\n";
    let diagnostics = lint_script_diagnostics_with(source, &resolver("recommended"));
    let spans: Vec<&str> = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == Code::L011)
        .map(|diagnostic| {
            let span = diagnostic.span.expect("L011 span");
            &source[span.start as usize..span.end as usize]
        })
        .collect();
    assert_eq!(
        spans,
        ["not a == b", "not (a == b)", "not ((a or b) == c)"],
        "{:?}",
        codes(&diagnostics)
    );

    let l011_fixes: Vec<&Fix> = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == Code::L011)
        .filter_map(|diagnostic| diagnostic.fix.as_ref())
        .collect();
    let fixed = apply_fixes(source, l011_fixes);
    assert!(!fixed.contains("not "), "{fixed}");
    let found = lint_script(&fixed, "recommended");
    assert!(!found.contains(&Code::L011), "{found:?}");
}

#[test]
fn l002_reports_a_destructuring_var_once_per_declaration() {
    let source = "fn main() {\n    var (a, b) = (0, 0)\n    print(\"{a}{b}\")\n}\n";
    let diagnostics = lint_script_diagnostics_with(source, &resolver("recommended"));
    let l002: Vec<_> = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == Code::L002)
        .collect();
    assert_eq!(l002.len(), 1, "{:?}", codes(&diagnostics));
    let fixed = apply_fix(source, l002[0].fix.as_ref().expect("L002 fix"));
    assert_eq!(fixed.matches("let").count(), 1, "{fixed}");
    assert!(!fixed.contains("var"), "{fixed}");
    let found = lint_script(&fixed, "recommended");
    assert!(!found.contains(&Code::L002), "{found:?}");
}

#[test]
fn l002_skips_a_destructuring_var_when_one_binding_is_mutated() {
    let source = "fn main() {\n    var (a, b) = (0, 0)\n    a += 1\n    print(\"{b}\")\n}\n";
    let found = lint_script(source, "recommended");
    assert!(!found.contains(&Code::L002), "{found:?}");
}

#[test]
fn l004_removal_never_eats_a_closing_brace() {
    let source = "fn f() {\n    panic(\"x\")\n    let x = 1 }\nfn main() { f() }\n";
    let diagnostics = lint_script_diagnostics_with(source, &resolver("recommended"));
    let fixed = apply_fix(source, fix_for(&diagnostics, Code::L004));
    assert!(fixed.contains("    }\n"), "{fixed}");
    assert!(fixed.contains("fn main() { f() }"), "{fixed}");
    let found = lint_script(&fixed, "recommended");
    assert!(!found.contains(&Code::L004), "{found:?}");
}

#[test]
fn l012_fix_keeps_a_parenthesized_rhs_balanced() {
    let source = "fn main() {\n    var x = 0\n    x = x + (1)\n    print(\"{x}\")\n}\n";
    let diagnostics = lint_script_diagnostics_with(source, &resolver("recommended"));
    let fixed = apply_fix(source, fix_for(&diagnostics, Code::L012));
    assert!(fixed.contains("x += (1)"), "{fixed}");
    let found = lint_script(&fixed, "recommended");
    assert!(!found.contains(&Code::L012), "{found:?}");
}

#[test]
fn l013_final_return_keeps_parenthesized_value_source() {
    let source = "fn f(a: int, b: int, c: int) -> int {\n    return (a + b) * c\n}\n\nfn main() {\n    print(\"{f(1, 2, 3)}\")\n}\n";
    let parsed = parse_source(source, Default::default());
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let settings = resolver("recommended").resolve("main.mt");
    let diagnostics = lint_syntax_only(SourceId(0), source, &parsed.program, &settings);
    let fixed = apply_fix(source, fix_for(&diagnostics, Code::L013));
    assert!(fixed.contains("    (a + b) * c\n"), "{fixed}");
    let found = lint_script(&fixed, "recommended");
    assert!(!found.contains(&Code::L013), "{found:?}");
}

#[test]
fn suppression_covers_the_head_of_a_compound_if() {
    let source =
        "fn main() {\n    return\n    // allow(L004)\n    if true { print(\"dead\") }\n}\n";
    let found = lint_script(source, "recommended");
    assert!(!found.contains(&Code::L004), "{found:?}");
    assert!(!found.contains(&Code::L022), "{found:?}");
}

#[test]
fn aliased_enum_variant_construction_counts_as_a_use() {
    let mut provider = MemoryProvider::new();
    provider.insert("m.mt", "pub enum Shape { Circle(float) }\n");
    provider.insert(
        "main.mt",
        "use m.Shape as S\n\nfn main() {\n    print(\"{S.Circle(1.0)}\")\n}\n",
    );
    let loaded = load(
        &provider,
        &LoadOptions {
            entry: "main.mt".into(),
            interfaces: Vec::new(),
            grants: Vec::new(),
            limits: Default::default(),
            edition: "1".into(),
        },
    );
    assert!(!loaded.has_errors(), "{:?}", codes(&loaded.diagnostics));
    let output = lint(
        loaded.program.as_ref().expect("checked program"),
        &loaded.sources,
        &resolver("recommended"),
        "",
        &LintInputs::default(),
    );
    assert!(
        !codes(&output.diagnostics).contains(&Code::L001),
        "{:?}",
        output.diagnostics
    );
}
