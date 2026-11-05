use std::collections::HashSet;

use martite_base::Code;
use martite_config::ToolConfig;
use martite_conformance::{Case, LintRun, lint_case};
use martite_lint::Tier;

const PLUGIN_INTERFACE: &str = r#"struct Doc
fn open() -> Doc
fn maybe() -> str?
on check()
on tick()
on add(xs: list[int]) -> Result[(), str]
on saved(item: str) -> Result[(), str]
"#;

const PLUGIN_MANIFEST: &str = r#"
[plugin]
name = "lint-fixture"
version = "0.1.0"
lang = "1"
api = "1.0"
entry = "main.mt"

[permissions]
grant = ["app.api"]
"#;

enum Fixture {
    Script(&'static str),
    Plugin {
        main: &'static str,
        interface: &'static str,
        grants: &'static [&'static str],
        manifest: Option<&'static str>,
    },
}

impl Fixture {
    fn case(&self) -> Case {
        match self {
            Fixture::Script(source) => Case::single(*source),
            Fixture::Plugin {
                main,
                interface,
                grants,
                ..
            } => {
                let mut case = Case::single(*main).interface("app.api", *interface);
                for grant in *grants {
                    case = case.grant(*grant);
                }
                case
            }
        }
    }

    fn manifest(&self) -> Option<&'static str> {
        match self {
            Fixture::Script(_) => None,
            Fixture::Plugin { manifest, .. } => *manifest,
        }
    }
}

struct LintRow {
    code: Code,
    tier: Tier,
    preset: &'static str,
    fail: Fixture,
    pass: Fixture,
    span: Option<&'static str>,
}

const fn script(source: &'static str) -> Fixture {
    Fixture::Script(source)
}

const fn plugin(main: &'static str) -> Fixture {
    Fixture::Plugin {
        main,
        interface: PLUGIN_INTERFACE,
        grants: &["app.api"],
        manifest: None,
    }
}

const ROWS: &[LintRow] = &[
    LintRow {
        code: Code::L001,
        tier: Tier::Flow,
        preset: "recommended",
        fail: script("fn main() {\n    let unused = 1\n}\n"),
        pass: script("fn main() {\n    let used = 1\n    print(used)\n}\n"),
        span: Some("unused"),
    },
    LintRow {
        code: Code::L002,
        tier: Tier::Resolved,
        preset: "recommended",
        fail: script("fn main() {\n    var total = 0\n    print(total)\n}\n"),
        pass: script("fn main() {\n    let total = 0\n    print(total)\n}\n"),
        span: Some("var"),
    },
    LintRow {
        code: Code::L003,
        tier: Tier::Patterns,
        preset: "recommended",
        fail: script(
            "fn main() {\n    match true {\n        true => print(\"a\"),\n        false => print(\"b\"),\n        _ => print(\"c\"),\n    }\n}\n",
        ),
        pass: script(
            "fn main() {\n    match true {\n        true => print(\"a\"),\n        false => print(\"b\"),\n    }\n}\n",
        ),
        span: Some("_"),
    },
    LintRow {
        code: Code::L004,
        tier: Tier::Typed,
        preset: "recommended",
        fail: script("fn main() {\n    return\n    print(\"never\")\n}\n"),
        pass: script("fn main() {\n    print(\"once\")\n}\n"),
        span: Some("print(\"never\")"),
    },
    LintRow {
        code: Code::L005,
        tier: Tier::Syntax,
        preset: "recommended",
        fail: script(
            "fn check(flag: bool) {\n    if true {\n        print(\"always\")\n    }\n}\nfn main() {\n    check(true)\n}\n",
        ),
        pass: script(
            "fn check(flag: bool) {\n    if flag {\n        print(\"maybe\")\n    }\n}\nfn main() {\n    check(true)\n}\n",
        ),
        span: Some("true"),
    },
    LintRow {
        code: Code::L006,
        tier: Tier::Resolved,
        preset: "recommended",
        fail: script(
            "fn check(x: int) {\n    if x == x {\n        print(\"same\")\n    }\n}\nfn main() {\n    check(1)\n}\n",
        ),
        pass: script(
            "fn check(x: int) {\n    if x == 1 {\n        print(\"one\")\n    }\n}\nfn main() {\n    check(1)\n}\n",
        ),
        span: Some("=="),
    },
    LintRow {
        code: Code::L007,
        tier: Tier::Typed,
        preset: "recommended",
        fail: script(
            "fn check(x: float) {\n    if x == 0.5 {\n        print(\"half\")\n    }\n}\nfn main() {\n    check(0.5)\n}\n",
        ),
        pass: script(
            "fn check(x: float) {\n    if x == 0.0 {\n        print(\"zero\")\n    }\n}\nfn main() {\n    check(0.0)\n}\n",
        ),
        span: Some("=="),
    },
    LintRow {
        code: Code::L008,
        tier: Tier::Typed,
        preset: "recommended",
        fail: plugin(
            "use app.api\non check() {\n    let text = api.maybe().expect(\"missing\")\n    print(text)\n}\n",
        ),
        pass: plugin(
            "use app.api\non check() {\n    let words = [\"a\"]\n    if words.first().is_some() {\n        let word = words.first().expect(\"checked\")\n        print(word)\n    }\n}\n",
        ),
        span: Some("api.maybe().expect"),
    },
    LintRow {
        code: Code::L009,
        tier: Tier::Typed,
        preset: "recommended",
        fail: script(
            "fn fail() -> Result[int, str] {\n    Err(\"x\")\n}\nfn main() {\n    let _ = fail()\n}\n",
        ),
        pass: script("fn main() {\n    let _ = 1\n}\n"),
        span: Some("_"),
    },
    LintRow {
        code: Code::L010,
        tier: Tier::Typed,
        preset: "recommended",
        fail: script("fn main() {\n    let x = 10 / 0\n    print(x)\n}\n"),
        pass: script("fn main() {\n    let x = 10 / 2\n    print(x)\n}\n"),
        span: Some("0"),
    },
    LintRow {
        code: Code::L011,
        tier: Tier::Syntax,
        preset: "recommended",
        fail: script(
            "fn check(flag: bool) {\n    if flag == true {\n        print(\"yes\")\n    }\n}\nfn main() {\n    check(true)\n}\n",
        ),
        pass: script(
            "fn check(flag: bool) {\n    if flag {\n        print(\"yes\")\n    }\n}\nfn main() {\n    check(true)\n}\n",
        ),
        span: Some("flag == true"),
    },
    LintRow {
        code: Code::L012,
        tier: Tier::Resolved,
        preset: "recommended",
        fail: script(
            "fn main() {\n    var total = 0\n    total = total + 1\n    print(total)\n}\n",
        ),
        pass: script("fn main() {\n    var total = 0\n    total += 1\n    print(total)\n}\n"),
        span: Some("="),
    },
    LintRow {
        code: Code::L013,
        tier: Tier::Syntax,
        preset: "recommended",
        fail: script("fn done() {\n    return\n}\nfn main() {\n    done()\n}\n"),
        pass: script("fn done() {}\nfn main() {\n    done()\n}\n"),
        span: Some("return"),
    },
    LintRow {
        code: Code::L014,
        tier: Tier::Typed,
        preset: "recommended",
        fail: script(
            "fn choose(flag: bool) -> int {\n    match flag {\n        true => 1,\n        false => 0,\n    }\n}\nfn main() {\n    print(choose(true))\n}\n",
        ),
        pass: script(
            "fn choose(n: int) -> int {\n    match n {\n        1 => 1,\n        _ => 0,\n    }\n}\nfn main() {\n    print(choose(1))\n}\n",
        ),
        span: Some("flag"),
    },
    LintRow {
        code: Code::L015,
        tier: Tier::Resolved,
        preset: "recommended",
        fail: script(
            "fn show(mut value: int) {\n    print(value)\n}\nfn main() {\n    var value = 1\n    show(mut value)\n}\n",
        ),
        pass: script("fn show(value: int) {\n    print(value)\n}\nfn main() {\n    show(1)\n}\n"),
        span: Some("mut"),
    },
    LintRow {
        code: Code::L016,
        tier: Tier::Syntax,
        preset: "recommended",
        fail: script("fn main() {\n    if false {\n    }\n}\n"),
        pass: script("fn main() {\n    if false {\n        // deliberately empty\n    }\n}\n"),
        span: Some("{"),
    },
    LintRow {
        code: Code::L017,
        tier: Tier::Syntax,
        preset: "recommended",
        fail: script(
            "fn wide(a: int, b: int, c: int, d: int, e: int, f: int) {\n    print(a)\n}\nfn main() {\n    wide(1, 2, 3, 4, 5, 6)\n}\n",
        ),
        pass: script(
            "fn narrow(a: int, b: int) {\n    print(a)\n}\nfn main() {\n    narrow(1, 2)\n}\n",
        ),
        span: Some("wide"),
    },
    LintRow {
        code: Code::L018,
        tier: Tier::Syntax,
        preset: "strict",
        fail: script("pub fn exposed() {}\nfn main() {\n    exposed()\n}\n"),
        pass: script(
            "/// Public entry point.\npub fn exposed() {}\nfn main() {\n    exposed()\n}\n",
        ),
        span: Some("exposed"),
    },
    LintRow {
        code: Code::L019,
        tier: Tier::Flow,
        preset: "recommended",
        fail: plugin(
            "use app.api\nstruct State { n: int }\non init() -> State { State { n: 0 } }\non tick(mut self) {\n    while true {\n        self.n += 1\n    }\n}\n",
        ),
        pass: plugin(
            "use app.api\nstruct State { n: int }\non init() -> State { State { n: 0 } }\non tick(mut self) {\n    var running = true\n    while running {\n        running = false\n    }\n}\n",
        ),
        span: Some("while"),
    },
    LintRow {
        code: Code::L020,
        tier: Tier::Manifest,
        preset: "recommended",
        fail: Fixture::Plugin {
            main: "use app.api\non check() {}\n",
            interface: "on check()\n",
            grants: &["app.api", "std.math"],
            manifest: Some(
                "[plugin]\nname = \"lint-fixture\"\nversion = \"0.1.0\"\nlang = \"1\"\napi = \"1.0\"\nentry = \"main.mt\"\n\n[permissions]\ngrant = [\"app.api\", \"std.math\"]\n",
            ),
        },
        pass: Fixture::Plugin {
            main: "use app.api\non check() {}\n",
            interface: "on check()\n",
            grants: &["app.api"],
            manifest: Some(PLUGIN_MANIFEST),
        },
        span: Some("\"std.math\""),
    },
    LintRow {
        code: Code::L021,
        tier: Tier::Typed,
        preset: "recommended",
        fail: plugin(
            "use app.api\nuse app.api.Doc\nstruct State { doc: Doc }\non init() -> State { State { doc: api.open() } }\non check() {}\n",
        ),
        pass: plugin(
            "use app.api\nstruct State { n: int }\non init() -> State { State { n: 0 } }\non check() {}\n",
        ),
        span: Some("Doc"),
    },
    LintRow {
        code: Code::L022,
        tier: Tier::Post,
        preset: "recommended",
        fail: script("// allow(L999)\nfn main() {\n    print(\"x\")\n}\n"),
        pass: script(
            "fn main() {\n    // allow(L005)\n    if true {\n        print(\"x\")\n    }\n}\n",
        ),
        span: Some("// allow(L999)"),
    },
    LintRow {
        code: Code::L023,
        tier: Tier::Typed,
        preset: "recommended",
        fail: script(
            "fn compute() -> int { 1 }\nfn main() {\n    let maybe: int? = None\n    let value = maybe.unwrap_or(compute())\n    print(value)\n}\n",
        ),
        pass: script(
            "fn main() {\n    let maybe: int? = None\n    let value = maybe.unwrap_or(0)\n    print(value)\n}\n",
        ),
        span: Some("maybe.unwrap_or(compute())"),
    },
    LintRow {
        code: Code::L024,
        tier: Tier::Syntax,
        preset: "recommended",
        fail: script(
            "fn check(a: bool, b: bool, c: bool) {\n    if a and b or c {\n        print(\"yes\")\n    }\n}\nfn main() {\n    check(true, true, false)\n}\n",
        ),
        pass: script(
            "fn check(a: bool, b: bool, c: bool) {\n    if (a and b) or c {\n        print(\"yes\")\n    }\n}\nfn main() {\n    check(true, true, false)\n}\n",
        ),
        span: Some("or"),
    },
    LintRow {
        code: Code::L025,
        tier: Tier::Typed,
        preset: "recommended",
        fail: script(
            "fn main() {\n    var text = \"\"\n    for word in [\"a\", \"b\"] {\n        text += word\n    }\n    print(text)\n}\n",
        ),
        pass: script("fn main() {\n    let text = [\"a\", \"b\"].join(\"\")\n    print(text)\n}\n"),
        span: Some("+="),
    },
    LintRow {
        code: Code::L026,
        tier: Tier::Flow,
        preset: "strict",
        fail: plugin(
            "struct State { n: int }\non init() -> State { State { n: 0 } }\non add(mut self, xs: list[int]) -> Result[(), str] {\n    self.n += xs[0]\n    Ok(())\n}\n",
        ),
        pass: plugin(
            "struct State { n: int }\non init() -> State { State { n: 0 } }\non add(mut self, xs: list[int]) -> Result[(), str] {\n    let first = xs.get(0) ?? 0\n    self.n = first\n    Ok(())\n}\n",
        ),
        span: Some("+="),
    },
    LintRow {
        code: Code::L027,
        tier: Tier::Flow,
        preset: "recommended",
        fail: plugin(
            "use app.api\nstruct State { n: int }\non init() -> State { State { n: 0 } }\non saved(mut self, item: str) -> Result[(), str] {\n    self.n += 1\n    if item == \"\" {\n        return Err(\"empty\")\n    }\n    Ok(())\n}\n",
        ),
        pass: plugin(
            "use app.api\nstruct State { n: int }\non init() -> State { State { n: 0 } }\non saved(mut self, item: str) -> Result[(), str] {\n    if item == \"\" {\n        return Err(\"empty\")\n    }\n    self.n += 1\n    Ok(())\n}\n",
        ),
        span: Some("self.n += 1"),
    },
    LintRow {
        code: Code::L028,
        tier: Tier::Resolved,
        preset: "recommended",
        fail: script("fn main() {\n    let _count = 1\n    print(_count)\n}\n"),
        pass: script("fn main() {\n    let count = 1\n    print(count)\n}\n"),
        span: Some("_count"),
    },
    LintRow {
        code: Code::L029,
        tier: Tier::Resolved,
        preset: "recommended",
        fail: script(
            "fn show(x: int) {\n    print(x)\n}\nfn main() {\n    let f = fn(x: int) => show(x)\n    f(1)\n}\n",
        ),
        pass: script(
            "fn show(x: int) {\n    print(x)\n}\nfn main() {\n    let f = show\n    f(1)\n}\n",
        ),
        span: Some("fn(x: int) => show(x)"),
    },
    LintRow {
        code: Code::L030,
        tier: Tier::Flow,
        preset: "recommended",
        fail: script("test \"empty\" {\n    let value = 1\n}\n"),
        pass: script("test \"checked\" {\n    assert(true, \"ok\")\n}\n"),
        span: Some("empty"),
    },
    LintRow {
        code: Code::L031,
        tier: Tier::Typed,
        preset: "strict",
        fail: plugin("use app.api\non check() {\n    print(\"debug\")\n}\n"),
        pass: plugin("use app.api\non check() {\n    let value = 1\n}\n"),
        span: Some("print"),
    },
    LintRow {
        code: Code::L032,
        tier: Tier::Syntax,
        preset: "recommended",
        fail: script("fn main() {\n    /// stray\n    let value = 1\n    print(value)\n}\n"),
        pass: script(
            "/// Useful documentation.\npub fn documented() {}\nfn main() {\n    documented()\n}\n",
        ),
        span: Some("/// stray"),
    },
];

fn config(preset: &str) -> ToolConfig {
    ToolConfig {
        lint_preset: Some(preset.into()),
        ..ToolConfig::default()
    }
}

fn run_fixture(fixture: &Fixture, preset: &str) -> LintRun {
    lint_case(&fixture.case(), &config(preset), None, fixture.manifest())
}

#[test]
fn every_lint_rule_has_a_fail_and_pass_fixture_at_its_tier() {
    let mut failures = Vec::new();
    for row in ROWS {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let fail = run_fixture(&row.fail, row.preset);
            fail.assert_has(row.code, row.span, None);
            let pass = run_fixture(&row.pass, row.preset);
            pass.assert_not(row.code);
        }));
        if let Err(payload) = result {
            let message = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "unknown panic".into());
            failures.push(format!("{}: {message}", row.code));
        }
    }
    assert!(
        failures.is_empty(),
        "lint fixtures failed:\n{}",
        failures.join("\n\n")
    );
}

#[test]
fn registry_tiers_and_defaults_match_the_specification() {
    use martite_base::{LintGroup, Severity};
    use martite_lint::{Level, RULES};

    let expected: &[(Code, Tier, LintGroup, Level)] = &[
        (Code::L001, Tier::Flow, LintGroup::Style, Level::Warn),
        (Code::L002, Tier::Resolved, LintGroup::Simplify, Level::Warn),
        (Code::L003, Tier::Patterns, LintGroup::Correct, Level::Warn),
        (Code::L004, Tier::Typed, LintGroup::Correct, Level::Warn),
        (Code::L005, Tier::Syntax, LintGroup::Correct, Level::Warn),
        (Code::L006, Tier::Resolved, LintGroup::Correct, Level::Warn),
        (Code::L007, Tier::Typed, LintGroup::Correct, Level::Warn),
        (Code::L008, Tier::Typed, LintGroup::Plugin, Level::Warn),
        (Code::L009, Tier::Typed, LintGroup::Correct, Level::Warn),
        (Code::L010, Tier::Typed, LintGroup::Correct, Level::Error),
        (Code::L011, Tier::Syntax, LintGroup::Simplify, Level::Info),
        (Code::L012, Tier::Resolved, LintGroup::Simplify, Level::Info),
        (Code::L013, Tier::Syntax, LintGroup::Simplify, Level::Info),
        (Code::L014, Tier::Typed, LintGroup::Simplify, Level::Info),
        (Code::L015, Tier::Resolved, LintGroup::Correct, Level::Warn),
        (Code::L016, Tier::Syntax, LintGroup::Style, Level::Info),
        (Code::L017, Tier::Syntax, LintGroup::Style, Level::Info),
        (Code::L018, Tier::Syntax, LintGroup::Docs, Level::Off),
        (Code::L019, Tier::Flow, LintGroup::Plugin, Level::Error),
        (Code::L020, Tier::Manifest, LintGroup::Plugin, Level::Warn),
        (Code::L021, Tier::Typed, LintGroup::Plugin, Level::Warn),
        (Code::L022, Tier::Post, LintGroup::Correct, Level::Warn),
        (Code::L023, Tier::Typed, LintGroup::Perf, Level::Info),
        (Code::L024, Tier::Syntax, LintGroup::Style, Level::Info),
        (Code::L025, Tier::Typed, LintGroup::Perf, Level::Info),
        (Code::L026, Tier::Flow, LintGroup::Plugin, Level::Off),
        (Code::L027, Tier::Flow, LintGroup::Plugin, Level::Warn),
        (Code::L028, Tier::Resolved, LintGroup::Style, Level::Info),
        (Code::L029, Tier::Resolved, LintGroup::Simplify, Level::Info),
        (Code::L030, Tier::Flow, LintGroup::Correct, Level::Warn),
        (Code::L031, Tier::Typed, LintGroup::Plugin, Level::Info),
        (Code::L032, Tier::Syntax, LintGroup::Docs, Level::Warn),
    ];
    assert_eq!(RULES.len(), expected.len());
    for (code, tier, group, default) in expected {
        let rule = martite_lint::rule(*code).unwrap_or_else(|| panic!("missing {code}"));
        assert_eq!(rule.tier, *tier, "tier for {code}");
        assert_eq!(rule.group, *group, "group for {code}");
        assert_eq!(rule.default, *default, "default for {code}");
        assert!(
            rule.default.severity() != Some(Severity::Error) || rule.default == Level::Error,
            "{code}"
        );
    }
}

#[test]
fn syntax_only_runs_only_syntax_tier_rules() {
    for row in ROWS {
        let fixture = row.fail.case();
        let parsed = martite_syntax::parse_source(
            match &row.fail {
                Fixture::Script(source) => source,
                Fixture::Plugin { main, .. } => main,
            },
            Default::default(),
        );
        if !parsed.diagnostics.is_empty() {
            continue;
        }
        let resolver = martite_lint::SettingsResolver::new(&config(row.preset), None).unwrap();
        let settings = resolver.resolve("main.mt");
        let diagnostics = martite_lint::lint_syntax_only(
            martite_base::SourceId(0),
            match &row.fail {
                Fixture::Script(source) => source,
                Fixture::Plugin { main, .. } => main,
            },
            &parsed.program,
            &settings,
        );
        let found = diagnostics.iter().any(|d| d.code == row.code);
        assert_eq!(
            found,
            row.tier == Tier::Syntax,
            "{} should {} fire under --syntax-only",
            row.code,
            if row.tier == Tier::Syntax { "" } else { "not" }
        );
        let _ = fixture;
    }
}

#[test]
fn rule_selection_does_not_change_severity_or_suppress_l022() {
    let source = "fn main() {\n    // allow(L002)\n    var value = 1\n    print(value)\n}\n";
    let config = config("recommended");
    let selected = HashSet::from([Code::L005, Code::L022]);
    let run = lint_case(&Case::single(source), &config, Some(selected), None);
    run.assert_not(Code::L022);
}
