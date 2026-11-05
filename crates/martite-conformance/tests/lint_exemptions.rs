use martite_base::Code;
use martite_config::ToolConfig;
use martite_conformance::{Case, LintRun, lint_case};

const INTERFACE: &str = r#"struct Doc
fn maybe() -> str?
fn open() -> Doc
on check()
on add(xs: list[int]) -> Result[(), str]
on saved(item: str) -> Result[(), str]
on tick()
"#;

fn config(preset: &str) -> ToolConfig {
    ToolConfig {
        lint_preset: Some(preset.into()),
        ..ToolConfig::default()
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Script,
    Plugin,
}

struct Exemption {
    name: &'static str,
    rule: Code,
    preset: &'static str,
    kind: Kind,
    source: &'static str,
}

fn run(row: &Exemption) -> LintRun {
    let case = match row.kind {
        Kind::Script => Case::single(row.source),
        Kind::Plugin => Case::single(row.source)
            .interface("app.api", INTERFACE)
            .grant("app.api"),
    };
    lint_case(&case, &config(row.preset), None, None)
}

fn assert_exempt(run: &LintRun, rule: Code, name: &str) {
    assert!(
        run.find(rule).is_none(),
        "{name}: unexpected {rule}\n{}",
        run.diagnostics
            .iter()
            .map(|d| d.render(&run.sources))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

const EXEMPTIONS: &[Exemption] = &[
    Exemption {
        name: "L001 underscore prefix",
        rule: Code::L001,
        preset: "recommended",
        kind: Kind::Script,
        source: "fn main() { let _unused = 1 }\n",
    },
    Exemption {
        name: "L001 self is never flagged",
        rule: Code::L001,
        preset: "recommended",
        kind: Kind::Script,
        source: "struct S { x: int }\nimpl S { fn show(self) { print(self.x) } }\nfn main() { S { x: 1 }.show() }\n",
    },
    Exemption {
        name: "L001 uses in tests count",
        rule: Code::L001,
        preset: "recommended",
        kind: Kind::Script,
        source: "fn helper() -> int { 1 }\ntest \"uses\" { assert(helper() == 1, \"x\") }\n",
    },
    Exemption {
        name: "L002 passed as a mut argument",
        rule: Code::L002,
        preset: "recommended",
        kind: Kind::Script,
        source: "fn bump(mut x: int) { x += 1\nprint(x) }\nfn main() { var x = 1\nbump(mut x) }\n",
    },
    Exemption {
        name: "L002 mut self call on a projection",
        rule: Code::L002,
        preset: "recommended",
        kind: Kind::Script,
        source: "struct S { x: int }\nimpl S { fn bump(mut self) { self.x += 1 } }\nfn main() { var s = S { x: 1 }\ns.bump() }\n",
    },
    Exemption {
        name: "L003 guarded arm is never redundant",
        rule: Code::L003,
        preset: "recommended",
        kind: Kind::Script,
        source: "fn main() { match 1 { x if x > 0 => print(\"a\"), _ => print(\"b\") } }\n",
    },
    Exemption {
        name: "L004 break makes the following statement reachable",
        rule: Code::L004,
        preset: "recommended",
        kind: Kind::Script,
        source: "fn main() { while true { break }\nprint(\"after\") }\n",
    },
    Exemption {
        name: "L005 while true is exempt",
        rule: Code::L005,
        preset: "recommended",
        kind: Kind::Script,
        source: "fn main() { while true { break } }\n",
    },
    Exemption {
        name: "L005 SCREAMING const is a deliberate switch",
        rule: Code::L005,
        preset: "recommended",
        kind: Kind::Script,
        source: "const DEBUG: bool = true\nfn main() { if DEBUG { print(\"x\") } }\n",
    },
    Exemption {
        name: "L006 calls are not side-effect-free",
        rule: Code::L006,
        preset: "recommended",
        kind: Kind::Script,
        source: "fn f() -> int { 1 }\nfn main() { if f() == f() { print(\"x\") } }\n",
    },
    Exemption {
        name: "L007 comparison with zero is allowed",
        rule: Code::L007,
        preset: "recommended",
        kind: Kind::Script,
        source: "fn check(x: float) { if x == 0.0 { print(\"zero\") } }\nfn main() { check(0.0) }\n",
    },
    Exemption {
        name: "L008 test blocks are exempt",
        rule: Code::L008,
        preset: "recommended",
        kind: Kind::Plugin,
        source: "use app.api\ntest \"t\" { let text = api.maybe().expect(\"x\")\nassert(true, \"x\") }\n",
    },
    Exemption {
        name: "L009 test blocks are exempt",
        rule: Code::L009,
        preset: "recommended",
        kind: Kind::Script,
        source: "fn fail() -> Result[int, str] { Err(\"x\") }\ntest \"t\" { let _ = fail()\nassert(true, \"x\") }\n",
    },
    Exemption {
        name: "L010 float division by zero is legal",
        rule: Code::L010,
        preset: "recommended",
        kind: Kind::Script,
        source: "fn main() { let x = 1.0 / 0.0\nprint(x) }\n",
    },
    Exemption {
        name: "L011 not (a < b) is not simplified",
        rule: Code::L011,
        preset: "recommended",
        kind: Kind::Script,
        source: "fn check(a: int, b: int) { if not (a < b) { print(\"x\") } }\nfn main() { check(1, 2) }\n",
    },
    Exemption {
        name: "L012 x = n + x is not rewritten",
        rule: Code::L012,
        preset: "recommended",
        kind: Kind::Script,
        source: "fn main() { var x = 1\nx = 2 + x\nprint(x) }\n",
    },
    Exemption {
        name: "L012 called index is evaluated twice",
        rule: Code::L012,
        preset: "recommended",
        kind: Kind::Script,
        source: "fn next() -> int { 0 }\nfn main() { var xs = [1, 2]\nxs[next()] = xs[next()] + 1\nprint(xs) }\n",
    },
    Exemption {
        name: "L013 return inside a loop is not final",
        rule: Code::L013,
        preset: "recommended",
        kind: Kind::Script,
        source: "fn f() { while true { return } }\nfn main() { f() }\n",
    },
    Exemption {
        name: "L015 mutated parameter keeps mut",
        rule: Code::L015,
        preset: "recommended",
        kind: Kind::Script,
        source: "fn bump(mut x: int) { x += 1\nprint(x) }\nfn main() { var x = 1\nbump(mut x) }\n",
    },
    Exemption {
        name: "L016 block with a comment is not empty",
        rule: Code::L016,
        preset: "recommended",
        kind: Kind::Script,
        source: "fn main() { if false { // deliberately empty\n } }\n",
    },
    Exemption {
        name: "L016 unit arm is explicit",
        rule: Code::L016,
        preset: "recommended",
        kind: Kind::Script,
        source: "fn main() { match 1 { _ => () } }\n",
    },
    Exemption {
        name: "L018 fields and variants are exempt",
        rule: Code::L018,
        preset: "strict",
        kind: Kind::Script,
        source: "/// docs\npub struct S { x: int }\n/// docs\npub enum E { A }\nfn main() { print(S { x: 1 }) }\n",
    },
    Exemption {
        name: "L019 return exits the loop",
        rule: Code::L019,
        preset: "recommended",
        kind: Kind::Plugin,
        source: "use app.api\nstruct State { n: int }\non init() -> State { State { n: 0 } }\non tick(mut self) { while true { return } }\n",
    },
    Exemption {
        name: "L021 no state type means no handle rule",
        rule: Code::L021,
        preset: "recommended",
        kind: Kind::Plugin,
        source: "use app.api\nuse app.api.Doc\non check() {}\n",
    },
    Exemption {
        name: "L023 plain name default is lazy enough",
        rule: Code::L023,
        preset: "recommended",
        kind: Kind::Script,
        source: "fn main() { let default = 0\nlet maybe: int? = None\nlet value = maybe.unwrap_or(default)\nprint(value) }\n",
    },
    Exemption {
        name: "L023 Result unwrap_or is exempt",
        rule: Code::L023,
        preset: "recommended",
        kind: Kind::Script,
        source: "fn main() { let result: Result[int, str] = Ok(1)\nlet value = result.unwrap_or(0)\nprint(value) }\n",
    },
    Exemption {
        name: "L025 concatenation outside a loop is fine",
        rule: Code::L025,
        preset: "recommended",
        kind: Kind::Script,
        source: "fn main() { var text = \"\"\ntext += \"a\"\nprint(text) }\n",
    },
    Exemption {
        name: "L026 assert is a deliberate check",
        rule: Code::L026,
        preset: "strict",
        kind: Kind::Plugin,
        source: "use app.api\non add(xs: list[int]) -> Result[(), str] { assert(xs.len() > 0, \"empty\")\nOk(()) }\n",
    },
    Exemption {
        name: "L026 expect is covered by L008",
        rule: Code::L026,
        preset: "strict",
        kind: Kind::Plugin,
        source: "use app.api\non check() { let text = api.maybe().expect(\"x\")\nprint(text) }\n",
    },
    Exemption {
        name: "L027 non-Result handlers are exempt",
        rule: Code::L027,
        preset: "recommended",
        kind: Kind::Plugin,
        source: "use app.api\nstruct State { n: int }\non init() -> State { State { n: 0 } }\non check(mut self) { self.n += 1\nreturn }\n",
    },
    Exemption {
        name: "L029 variant constructors are not free functions",
        rule: Code::L029,
        preset: "recommended",
        kind: Kind::Script,
        source: "fn main() { let f = fn(x: int) => Some(x)\nprint(f(1)) }\n",
    },
    Exemption {
        name: "L030 asserts through a free function count",
        rule: Code::L030,
        preset: "recommended",
        kind: Kind::Script,
        source: "fn check() { assert(true, \"x\") }\ntest \"t\" { check() }\n",
    },
    Exemption {
        name: "L030 handler call followed by an assert counts",
        rule: Code::L030,
        preset: "recommended",
        kind: Kind::Plugin,
        source: "use app.api\non check() {}\ntest \"t\" { check()\nassert(true, \"x\") }\n",
    },
    Exemption {
        name: "L031 tests may print",
        rule: Code::L031,
        preset: "strict",
        kind: Kind::Plugin,
        source: "use app.api\ntest \"t\" { print(\"debug\")\nassert(true, \"x\") }\n",
    },
    Exemption {
        name: "L013 return inside a match arm is not final",
        rule: Code::L013,
        preset: "recommended",
        kind: Kind::Script,
        source: "fn check(n: int) {\n    match n {\n        _ => { return },\n    }\n}\nfn main() { check(1) }\n",
    },
    Exemption {
        name: "L015 mutated self keeps mut",
        rule: Code::L015,
        preset: "recommended",
        kind: Kind::Script,
        source: "struct S { x: int }\nimpl S { fn bump(mut self) { self.x += 1 } }\nfn main() { var s = S { x: 1 }\ns.bump() }\n",
    },
    Exemption {
        name: "L016 struct braces are not a block",
        rule: Code::L016,
        preset: "recommended",
        kind: Kind::Script,
        source: "struct Empty {}\nfn main() { print(Empty {}) }\n",
    },
    Exemption {
        name: "L019 break exits the loop",
        rule: Code::L019,
        preset: "recommended",
        kind: Kind::Plugin,
        source: "use app.api\nstruct State { n: int }\non init() -> State { State { n: 0 } }\non tick(mut self) { while true { break } }\n",
    },
    Exemption {
        name: "L023 projection default is lazy enough",
        rule: Code::L023,
        preset: "recommended",
        kind: Kind::Script,
        source: "struct S { default: int }\nimpl S { fn value(self, maybe: int?) -> int { maybe.unwrap_or(self.default) } }\nfn main() { print(S { default: 0 }.value(None)) }\n",
    },
    Exemption {
        name: "L029 method calls are not free functions",
        rule: Code::L029,
        preset: "recommended",
        kind: Kind::Script,
        source: "fn main() { let xs = [1, 2]\nlet f = fn(x: int) => xs.contains(x)\nprint(f(1)) }\n",
    },
    Exemption {
        name: "L032 doc comments on fields and variants are attached",
        rule: Code::L032,
        preset: "recommended",
        kind: Kind::Script,
        source: "struct S {\n    /// field\n    x: int\n}\nenum E {\n    /// variant\n    A\n}\nfn main() { print(S { x: 1 }) }\n",
    },
    Exemption {
        name: "L032 doc comments on public items are attached",
        rule: Code::L032,
        preset: "recommended",
        kind: Kind::Script,
        source: "/// docs\npub fn exposed() {}\nfn main() { exposed() }\n",
    },
];

#[test]
fn documented_exemptions_do_not_fire() {
    let mut failures = Vec::new();
    for row in EXEMPTIONS {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let run = run(row);
            assert_exempt(&run, row.rule, row.name);
        }));
        if let Err(payload) = result {
            let message = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "unknown panic".into());
            failures.push(format!("{}: {message}", row.name));
        }
    }
    assert!(
        failures.is_empty(),
        "exemption fixtures failed:\n{}",
        failures.join("\n\n")
    );
}

#[test]
fn l017_test_blocks_are_exempt_from_the_line_limit() {
    let mut config = config("recommended");
    config.lint_thresholds.insert("max_fn_lines".into(), 1);
    let source =
        "test \"long\" {\n    let a = 1\n    let b = 2\n    assert(a + b == 3, \"x\")\n}\n";
    let run = lint_case(&Case::single(source), &config, None, None);
    assert_exempt(&run, Code::L017, "test line limit");
}

#[test]
fn l020_uses_inside_tests_count_as_using_the_grant() {
    let manifest = r#"
[plugin]
name = "lint-fixture"
version = "0.1.0"
lang = "1"
api = "1.0"
entry = "main.mt"

[permissions]
grant = ["app.api"]
"#;
    let case =
        Case::single("use app.api\ntest \"t\" { assert(api.maybe() == Some(\"x\"), \"x\") }\n")
            .interface("app.api", "fn maybe() -> str?\n")
            .grant("app.api");
    let run = lint_case(&case, &config("recommended"), None, Some(manifest));
    assert_exempt(&run, Code::L020, "grant used in a test");
}
