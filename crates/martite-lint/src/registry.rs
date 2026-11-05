//! Fixed rule registry for the linter.

use std::collections::HashSet;

use martite_base::{Code, LintGroup, Severity};

/// The analysis tier a rule needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Syntax,
    Resolved,
    Typed,
    Patterns,
    Flow,
    Manifest,
    Post,
}

impl Tier {
    pub const fn as_str(self) -> &'static str {
        match self {
            Tier::Syntax => "syntax",
            Tier::Resolved => "resolved",
            Tier::Typed => "typed",
            Tier::Patterns => "patterns",
            Tier::Flow => "flow",
            Tier::Manifest => "manifest",
            Tier::Post => "post",
        }
    }
}

/// A configured severity. `Off` is not a diagnostic severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Off,
    Error,
    Warn,
    Info,
}

impl Level {
    pub const fn as_str(self) -> &'static str {
        match self {
            Level::Off => "off",
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
        }
    }

    pub fn parse(value: &str) -> Option<Level> {
        Some(match value {
            "off" => Level::Off,
            "error" => Level::Error,
            "warn" | "warning" => Level::Warn,
            "info" => Level::Info,
            _ => return None,
        })
    }

    pub const fn is_enabled(self) -> bool {
        !matches!(self, Level::Off)
    }

    pub const fn severity(self) -> Option<Severity> {
        Some(match self {
            Level::Off => return None,
            Level::Error => Severity::Error,
            Level::Warn => Severity::Warning,
            Level::Info => Severity::Info,
        })
    }
}

impl std::fmt::Display for Level {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RuleMeta {
    pub code: Code,
    pub tier: Tier,
    pub group: LintGroup,
    pub default: Level,
    pub summary: &'static str,
    pub rationale: &'static str,
    pub example: &'static str,
}

macro_rules! rule {
    ($code:ident, $tier:ident, $group:ident, $default:ident, $summary:expr, $rationale:expr, $example:expr) => {
        RuleMeta {
            code: Code::$code,
            tier: Tier::$tier,
            group: LintGroup::$group,
            default: Level::$default,
            summary: $summary,
            rationale: $rationale,
            example: $example,
        }
    };
}

pub const RULES: &[RuleMeta] = &[
    rule!(
        L001,
        Flow,
        Style,
        Warn,
        "an unused binding, parameter, import, or private item",
        "Dead names make a program harder to read and hide mistakes. Imports are safe to delete; bindings and items may need a human decision.",
        "fn main() {\n    let unused = 1\n}\n"
    ),
    rule!(
        L002,
        Resolved,
        Simplify,
        Warn,
        "a `var` that is never mutated",
        "`let` states the binding cannot change. Using `var` when nothing mutates it makes the code look more dynamic than it is.",
        "fn main() {\n    var total = 0\n    print(total)\n}\n"
    ),
    rule!(
        L003,
        Patterns,
        Correct,
        Warn,
        "a redundant or unreachable match arm",
        "An arm that can never be selected is either dead code or a sign that an earlier arm is too broad.",
        "fn classify(b: bool) -> int {\n    match b {\n        true => 1,\n        false => 0,\n        _ => 2,\n    }\n}\n"
    ),
    rule!(
        L004,
        Typed,
        Correct,
        Warn,
        "an unreachable statement",
        "Statements after `return`, `break`, `continue`, `panic`, or a non-terminating `while true` never run.",
        "fn f() -> int {\n    return 1\n    print(\"never\")\n}\n"
    ),
    rule!(
        L005,
        Syntax,
        Correct,
        Warn,
        "a constant condition",
        "A condition that is always true or false is usually a leftover from debugging or a mistaken literal.",
        "fn main() {\n    if true {\n        print(\"always\")\n    }\n}\n"
    ),
    rule!(
        L006,
        Resolved,
        Correct,
        Warn,
        "a self-comparison or self-assignment",
        "Comparing or assigning a place to itself is almost always a typo. Calls are excluded because they can have effects.",
        "fn main(x: int) {\n    if x == x {\n        print(\"same\")\n    }\n}\n"
    ),
    rule!(
        L007,
        Typed,
        Correct,
        Warn,
        "an equality comparison on floats",
        "IEEE 754 equality is exact. Most code wants a tolerance, except for the deliberate `== 0.0` check.",
        "fn main(x: float) {\n    if x == 0.5 {\n        print(\"half\")\n    }\n}\n"
    ),
    rule!(
        L008,
        Typed,
        Plugin,
        Warn,
        "an unguarded `expect` in plugin code",
        "A failed `expect` traps the whole hook. Plugin code should return `Err` instead of aborting the host operation.",
        "on save(mut self, text: str) -> Result[(), str] {\n    let first = text.lines().first().expect(\"empty\")\n    Ok(())\n}\n"
    ),
    rule!(
        L009,
        Typed,
        Correct,
        Warn,
        "a discarded `Result`",
        "`let _ = result` throws away a recoverable error. Use `?`, a `match`, or an explicit error check.",
        "fn main() {\n    let _ = parse_int(\"x\")\n}\n"
    ),
    rule!(
        L010,
        Typed,
        Correct,
        Error,
        "integer division or remainder by a literal zero",
        "Integer division by zero always panics. The compiler knows the divisor is zero.",
        "fn main() {\n    let x = 10 / 0\n}\n"
    ),
    rule!(
        L011,
        Syntax,
        Simplify,
        Info,
        "a boolean expression or branch that can be simplified",
        "Comparing with `true` and `false`, and returning boolean literals from both branches, hides the underlying condition.",
        "fn main(ok: bool) {\n    if ok == true {\n        print(\"yes\")\n    }\n}\n"
    ),
    rule!(
        L012,
        Resolved,
        Simplify,
        Info,
        "`x = x + n` can be written as `x += n`",
        "The compound assignment evaluates the place once and states the intent directly.",
        "fn main() {\n    var total = 0\n    total = total + 1\n}\n"
    ),
    rule!(
        L013,
        Syntax,
        Simplify,
        Info,
        "a redundant final `return` or `else`",
        "The last expression of a body is its value, so a final `return` is noise. An `else` after a returning branch is also unnecessary.",
        "fn double(x: int) -> int {\n    return x * 2\n}\n"
    ),
    rule!(
        L014,
        Typed,
        Simplify,
        Info,
        "a `match` on a `bool`",
        "`if` states a two-way boolean choice more directly than a two-arm match.",
        "fn name(flag: bool) -> str {\n    match flag {\n        true => \"yes\",\n        false => \"no\",\n    }\n}\n"
    ),
    rule!(
        L015,
        Resolved,
        Correct,
        Warn,
        "a `mut` parameter or `mut self` that is never mutated",
        "`mut` promises a write. Leaving it on a parameter makes every caller pass a mutable place for no reason.",
        "fn show(mut value: int) {\n    print(value)\n}\n"
    ),
    rule!(
        L016,
        Syntax,
        Style,
        Info,
        "an empty block",
        "An empty block usually means a branch was left unfinished. `=> ()` is the explicit way to do nothing.",
        "fn main() {\n    if false {}\n}\n"
    ),
    rule!(
        L017,
        Syntax,
        Style,
        Info,
        "a function over a size threshold",
        "Long or deeply nested functions are hard to test and review. Split them or extract helpers.",
        "fn main() {\n    if true {\n        if true {\n            if true {\n                if true {\n                    print(\"deep\")\n                }\n            }\n        }\n    }\n}\n"
    ),
    rule!(
        L018,
        Syntax,
        Docs,
        Off,
        "a public item without a doc comment",
        "Public items are the plugin's API. A `///` comment explains the contract to callers.",
        "pub fn parse(text: str) -> int {\n    text.len()\n}\n"
    ),
    rule!(
        L019,
        Flow,
        Plugin,
        Error,
        "a `while true` with no exit",
        "In a handler or a function reachable from one, a loop with no exit can only end in `OutOfFuel`, which traps the hook.",
        "on tick(mut self) {\n    while true {\n        self.n += 1\n    }\n}\n"
    ),
    rule!(
        L020,
        Manifest,
        Plugin,
        Warn,
        "an unused manifest grant",
        "Granting a host module that the plugin never imports gives away authority it does not need.",
        "[permissions]\ngrant = [\"app.editor\", \"app.files\"]\n\n# main.mt only uses app.editor\n"
    ),
    rule!(
        L021,
        Typed,
        Plugin,
        Warn,
        "an opaque host handle in the plugin state",
        "Host handles are only valid for one host session. A snapshot restores them as stale values.",
        "struct State { doc: Doc }\n\non init() -> State {\n    State { doc: editor.open(\"a\") }\n}\n"
    ),
    rule!(
        L022,
        Post,
        Correct,
        Warn,
        "a suppression comment that does nothing",
        "Suppressions naming unknown rules, E-codes, or rules that never fire hide the fact that the comment is stale.",
        "// allow(L999)\nfn main() {}\n"
    ),
    rule!(
        L023,
        Typed,
        Perf,
        Info,
        "`unwrap_or` with an eager default",
        "`unwrap_or(d)` evaluates `d` even when the option is `Some`. Use `??` for a lazy default.",
        "fn main(maybe: int?) {\n    let value = maybe.unwrap_or(compute())\n}\n"
    ),
    rule!(
        L024,
        Syntax,
        Style,
        Info,
        "mixed `and` and `or` without parentheses",
        "Precedence is defined, but readers often misread `a and b or c`. Parentheses make the grouping explicit.",
        "fn main(a: bool, b: bool, c: bool) {\n    if a and b or c {\n        print(\"yes\")\n    }\n}\n"
    ),
    rule!(
        L025,
        Typed,
        Perf,
        Info,
        "repeated string concatenation inside a loop",
        "Each `+` copies the accumulated string, so the loop is quadratic. Collect into `list[str]` and call `join`.",
        "fn main() {\n    var text = \"\"\n    for word in [\"a\", \"b\"] {\n        text += word\n    }\n}\n"
    ),
    rule!(
        L026,
        Flow,
        Plugin,
        Off,
        "a panic-capable operation in plugin code",
        "A trap aborts the whole hook. Plugin code should handle the fallible case or return `Err`.",
        "on save(mut self, items: list[str]) -> Result[(), str] {\n    let first = items[0]\n    Ok(())\n}\n"
    ),
    rule!(
        L027,
        Flow,
        Plugin,
        Warn,
        "state is mutated before a fallible return",
        "`Err` is a normal return and commits state. Validate before mutating so a failed hook leaves the state untouched.",
        "on add(mut self, item: str) -> Result[(), str] {\n    self.count += 1\n    if item == \"\" {\n        return Err(\"empty\")\n    }\n    Ok(())\n}\n"
    ),
    rule!(
        L028,
        Resolved,
        Style,
        Info,
        "a `_`-prefixed binding is read",
        "The prefix says the value is intentionally unused, but the body reads it. Rename it to make the intent honest.",
        "fn main() {\n    let _count = 1\n    print(_count)\n}\n"
    ),
    rule!(
        L029,
        Resolved,
        Simplify,
        Info,
        "a lambda that only forwards its arguments",
        "`fn(x) => f(x)` can be written as `f` when `f` is a free function.",
        "fn main() {\n    let show = fn(x: int) => print(x)\n}\n"
    ),
    rule!(
        L030,
        Flow,
        Correct,
        Warn,
        "a test block with no assertions",
        "A test that never asserts still passes when the code is wrong. Call `assert` directly or through a helper.",
        "test \"adds\" {\n    let value = 1 + 1\n}\n"
    ),
    rule!(
        L031,
        Typed,
        Plugin,
        Info,
        "`print` in plugin code outside tests",
        "The host's log sink may be a no-op. A leftover debug print is usually noise.",
        "on save() {\n    print(\"debug\")\n}\n"
    ),
    rule!(
        L032,
        Syntax,
        Docs,
        Warn,
        "a `///` comment that is not attached to a doc-able element",
        "A `///` that does not directly precede an item, method, field, or variant is an ordinary comment with a misleading prefix.",
        "fn main() {\n    /// not documentation\n    let value = 1\n}\n"
    ),
];

/// Rule IDs that are reserved as tombstones. Edition 1 has none yet.
pub const RETIRED: &[Code] = &[];

pub fn rule(code: Code) -> Option<&'static RuleMeta> {
    RULES.iter().find(|rule| rule.code == code)
}

pub fn is_retired(code: Code) -> bool {
    RETIRED.contains(&code)
}

pub fn is_known(code: Code) -> bool {
    rule(code).is_some() || is_retired(code)
}

/// Parse a comma-separated `--rule` list. Unknown and retired IDs are usage errors.
pub fn parse_selection(value: &str) -> Result<HashSet<Code>, String> {
    let mut out = HashSet::new();
    for part in value.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let Some(code) = Code::from_str(part) else {
            return Err(format!("unknown rule `{part}`"));
        };
        if !code.is_lint() {
            return Err(format!("`{part}` is a compile error code, not a lint rule"));
        }
        if is_retired(code) {
            return Err(format!("rule `{part}` is retired"));
        }
        if !is_known(code) {
            return Err(format!("unknown rule `{part}`"));
        }
        out.insert(code);
    }
    if out.is_empty() {
        return Err("--rule requires at least one rule ID".into());
    }
    Ok(out)
}

pub fn list_rules_text() -> String {
    let mut out = String::new();
    for rule in RULES {
        out.push_str(&format!(
            "{}  {}  {}\n",
            rule.code,
            rule.group.as_str(),
            rule.default.as_str()
        ));
    }
    for code in RETIRED {
        out.push_str(&format!("{}  retired\n", code));
    }
    out
}

pub fn list_rules_json() -> String {
    let mut out = String::from("[");
    let mut first = true;
    for rule in RULES {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&format!(
            "{{\"id\":\"{}\",\"group\":\"{}\",\"default_severity\":\"{}\",\"retired\":false}}",
            rule.code,
            rule.group.as_str(),
            rule.default.as_str()
        ));
    }
    for code in RETIRED {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&format!(
            "{{\"id\":\"{}\",\"group\":\"\",\"default_severity\":\"off\",\"retired\":true}}",
            code
        ));
    }
    out.push(']');
    out
}

pub fn explain_text(code: Code) -> Option<String> {
    let rule = rule(code)?;
    Some(format!(
        "{} ({}, {}, {})\n{}\n\n{}\n\nExample:\n{}",
        rule.code,
        rule.group.as_str(),
        rule.default.as_str(),
        rule.tier.as_str(),
        rule.summary,
        rule.rationale,
        rule.example
    ))
}
