use martite_base::{Code, LintGroup};
use martite_config::{ToolConfig, parse_tool_config};
use martite_conformance::{Case, lint_case};
use martite_lint::{Level, SettingsResolver, glob_match};

fn resolver(text: &str) -> SettingsResolver {
    let config = parse_tool_config(text).expect("valid martite.toml");
    SettingsResolver::new(&config, None).expect("valid settings")
}

fn level(text: &str, path: &str, code: Code) -> Level {
    resolver(text).resolve(path).level(code)
}

#[test]
fn preset_precedence_is_golden() {
    let minimal = r#"
        [lint]
        preset = "minimal"
    "#;
    assert_eq!(level(minimal, "main.mt", Code::L003), Level::Warn);
    assert_eq!(level(minimal, "main.mt", Code::L010), Level::Error);
    assert_eq!(level(minimal, "main.mt", Code::L019), Level::Error);
    assert_eq!(level(minimal, "main.mt", Code::L001), Level::Off);
    assert_eq!(level(minimal, "main.mt", Code::L002), Level::Off);

    let recommended = r#"
        [lint]
        preset = "recommended"
    "#;
    assert_eq!(level(recommended, "main.mt", Code::L001), Level::Warn);
    assert_eq!(level(recommended, "main.mt", Code::L010), Level::Error);
    assert_eq!(level(recommended, "main.mt", Code::L018), Level::Off);
    assert_eq!(level(recommended, "main.mt", Code::L026), Level::Off);

    let strict = r#"
        [lint]
        preset = "strict"
    "#;
    assert_eq!(level(strict, "main.mt", Code::L018), Level::Warn);
    assert_eq!(level(strict, "main.mt", Code::L026), Level::Warn);
    assert_eq!(level(strict, "main.mt", Code::L010), Level::Error);
}

#[test]
fn group_and_severity_precedence_is_golden() {
    let text = r#"
        [lint]
        preset = "recommended"

        [lint.groups]
        simplify = "error"
        style = "off"

        [lint.severity]
        L016 = "warn"
        L002 = "info"
    "#;
    // Group first, then per-rule severity.
    assert_eq!(level(text, "main.mt", Code::L002), Level::Info);
    assert_eq!(level(text, "main.mt", Code::L016), Level::Warn);
    assert_eq!(level(text, "main.mt", Code::L011), Level::Error);
    assert_eq!(level(text, "main.mt", Code::L001), Level::Off);
}

#[test]
fn thresholds_and_options_are_golden() {
    let text = r#"
        [lint.thresholds]
        max_fn_lines = 2
        max_params = 1
        max_nesting = 0
        max_branches = 1

        [lint.options]
        float_eq_zero_ok = false
    "#;
    let settings = resolver(text).resolve("main.mt");
    assert_eq!(settings.thresholds.max_fn_lines, 2);
    assert_eq!(settings.thresholds.max_params, 1);
    assert_eq!(settings.thresholds.max_nesting, 0);
    assert_eq!(settings.thresholds.max_branches, 1);
    assert!(!settings.options.float_eq_zero_ok);
}

#[test]
fn float_eq_zero_option_changes_the_l007_fixture() {
    let source = "fn check(x: float) {\n    if x == 0.0 {\n        print(\"zero\")\n    }\n}\nfn main() { check(0.0) }\n";
    let default = lint_case(&Case::single(source), &ToolConfig::default(), None, None);
    default.assert_not(Code::L007);

    let mut config = ToolConfig::default();
    config
        .lint_options
        .insert("float_eq_zero_ok".into(), toml::Value::Boolean(false));
    let strict = lint_case(&Case::single(source), &config, None, None);
    strict.assert_has(Code::L007, Some("=="), None);
}

#[test]
fn thresholds_change_the_l017_fixture() {
    let source = "fn wide(a: int, b: int) {\n    print(a)\n}\nfn main() { wide(1, 2) }\n";
    let default = lint_case(&Case::single(source), &ToolConfig::default(), None, None);
    default.assert_not(Code::L017);

    let mut config = ToolConfig::default();
    config.lint_thresholds.insert("max_params".into(), 1);
    let strict = lint_case(&Case::single(source), &config, None, None);
    strict.assert_has(Code::L017, Some("wide"), None);
}

#[test]
fn override_order_within_one_entry_is_preset_groups_severity_off() {
    let text = r#"
        [lint]
        preset = "recommended"

        [[lint.override]]
        paths = ["src/**"]
        preset = "minimal"
        groups = { correct = "info" }
        severity = { L002 = "error" }
        off = ["L003"]
    "#;
    let settings = resolver(text).resolve("src/main.mt");
    assert_eq!(settings.level(Code::L001), Level::Off); // minimal preset
    assert_eq!(settings.level(Code::L003), Level::Off); // off wins
    assert_eq!(settings.level(Code::L010), Level::Info); // group override
    assert_eq!(settings.level(Code::L002), Level::Error); // severity override
}

#[test]
fn later_matching_overrides_win_and_non_matching_do_not_apply() {
    let text = r#"
        [lint]
        preset = "recommended"

        [[lint.override]]
        paths = ["src/**"]
        severity = { L002 = "error" }

        [[lint.override]]
        paths = ["src/lib/**"]
        severity = { L002 = "info" }

        [[lint.override]]
        paths = ["tests/**"]
        off = ["L005"]
    "#;
    assert_eq!(level(text, "src/main.mt", Code::L002), Level::Error);
    assert_eq!(level(text, "src/lib/a.mt", Code::L002), Level::Info);
    assert_eq!(level(text, "tests/a.mt", Code::L002), Level::Warn);
    assert_eq!(level(text, "tests/a.mt", Code::L005), Level::Off);
    assert_eq!(level(text, "main.mt", Code::L005), Level::Warn);
}

#[test]
fn path_globs_match_star_double_star_question_and_case() {
    assert!(glob_match("tests/**", "tests/a/b.mt"));
    assert!(glob_match("tests/*.mt", "tests/a.mt"));
    assert!(!glob_match("tests/*.mt", "tests/a/b.mt"));
    assert!(glob_match("**/*.mt", "a/b.mt"));
    assert!(glob_match("a?c.mt", "abc.mt"));
    assert!(!glob_match("A*.mt", "abc.mt"));
    assert!(glob_match("src\\**\\*.mt", "src/a/b.mt"));
    assert!(glob_match("./a/*.mt", "a/b.mt"));
    assert!(!glob_match("a/*.mt", "a/b/c.mt"));
}

#[test]
fn override_glob_precedence_uses_normalized_relative_paths() {
    let text = r#"
        [lint]
        preset = "recommended"

        [[lint.override]]
        paths = ["src/*.mt"]
        severity = { L002 = "error" }

        [[lint.override]]
        paths = ["src/**"]
        severity = { L002 = "warn" }
    "#;
    // Later matching entry wins.
    assert_eq!(level(text, "src/a.mt", Code::L002), Level::Warn);
    assert_eq!(level(text, "src/nested/a.mt", Code::L002), Level::Warn);
    assert_eq!(level(text, "other/a.mt", Code::L002), Level::Warn);
}

#[test]
fn invalid_thresholds_groups_and_rule_names_are_usage_errors() {
    for text in [
        "[lint.thresholds]\nmax_fn_lines = 0\n",
        "[lint.thresholds]\nmax_params = -1\n",
        "[lint.thresholds]\nmax_nesting = -1\n",
        "[lint.thresholds]\nmax_branches = 0\n",
        "[lint.groups]\nnope = \"warn\"\n",
        "[lint.groups]\nstyle = \"nope\"\n",
        "[lint.severity]\nL999 = \"warn\"\n",
        "[lint.severity]\nL001 = \"nope\"\n",
        "[lint.options]\nfloat_eq_zero_ok = \"yes\"\n",
    ] {
        let config = parse_tool_config(text).expect("TOML parses");
        assert!(
            SettingsResolver::new(&config, None).is_err(),
            "expected a usage error for {text:?}"
        );
    }
}

#[test]
fn override_requires_paths_and_rejects_conflicting_off_severity() {
    for text in [
        "[[lint.override]]\nseverity = { L001 = \"warn\" }\n",
        "[[lint.override]]\npaths = [\"main.mt\"]\noff = [\"L001\"]\nseverity = { L001 = \"warn\" }\n",
    ] {
        let config = parse_tool_config(text).expect("TOML parses");
        assert!(
            SettingsResolver::new(&config, None).is_err(),
            "expected an error for {text:?}"
        );
    }
}

#[test]
fn defaults_and_groups_are_exposed_for_documentation() {
    let config = ToolConfig::default();
    let resolver = SettingsResolver::new(&config, None).unwrap();
    let settings = resolver.resolve("main.mt");
    assert_eq!(settings.level(Code::L001), Level::Warn);
    assert!(LintGroup::from_str("plugin").is_some());
}
