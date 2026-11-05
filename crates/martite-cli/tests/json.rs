use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value as Json;

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(name: &str) -> Self {
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "martite-json-{}-{}-{}",
            std::process::id(),
            unique,
            name
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn file(&self, relative: &str, text: &str) -> PathBuf {
        let path = self.path.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(&path, text).expect("write fixture");
        path
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn martite(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_martite"))
        .args(args)
        .output()
        .expect("run martite")
}

fn martite_in(dir: &Path, args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_martite"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run martite")
}

fn s(value: &str) -> &std::ffi::OsStr {
    std::ffi::OsStr::new(value)
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

fn json_lines(output: &Output) -> Vec<Json> {
    stdout(output)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("valid JSON line"))
        .collect()
}

#[test]
fn lint_json_has_group_spans_help_and_fix_applicability() {
    let dir = TempDir::new("lint");
    let script = dir.file(
        "main.mt",
        "fn main() {\n    var value = 1\n    print(value)\n    match true {\n        true => print(\"a\"),\n        false => print(\"b\"),\n        _ => print(\"c\"),\n    }\n}\n",
    );
    let output = martite(&[
        s("lint"),
        s("--json"),
        s("--rule"),
        s("L002,L003"),
        script.as_os_str(),
    ]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let diagnostics = json_lines(&output);
    assert_eq!(diagnostics.len(), 2, "{diagnostics:#?}");

    let l002 = diagnostics
        .iter()
        .find(|d| d["code"] == "L002")
        .expect("L002");
    assert_eq!(l002["severity"], "warning");
    assert_eq!(l002["group"], "simplify");
    assert_eq!(l002["file"], "main.mt");
    assert_eq!(l002["span"]["line"], 2);
    assert!(l002["span"]["start"].as_u64().unwrap() < l002["span"]["end"].as_u64().unwrap());
    assert!(l002["help"].as_str().unwrap().contains("let"));
    assert_eq!(l002["fix"][0]["applicability"], "machine");
    assert_eq!(l002["fix"][0]["replacement"], "let");

    let l003 = diagnostics
        .iter()
        .find(|d| d["code"] == "L003")
        .expect("L003");
    assert_eq!(l003["group"], "correct");
    assert_eq!(l003["fix"][0]["applicability"], "maybe");
    assert!(l003["help"].as_str().is_some());
}

#[test]
fn check_json_has_byte_offsets_and_one_based_scalar_columns() {
    let dir = TempDir::new("check");
    let script = dir.file("main.mt", "fn main() {\n    let x: int = \"no\"\n}\n");
    let output = martite(&[s("check"), s("--json"), script.as_os_str()]);
    assert_eq!(output.status.code(), Some(1));
    let diagnostics = json_lines(&output);
    let e201 = diagnostics.iter().find(|d| d["code"] == "E201").unwrap();
    assert_eq!(e201["file"], "main.mt");
    assert_eq!(e201["span"]["line"], 2);
    assert_eq!(e201["span"]["column"], 18);
    assert!(e201["span"]["start"].as_u64().is_some());
    assert!(e201["span"]["end"].as_u64().is_some());
}

#[test]
fn list_rules_json_is_an_array_with_stable_fields() {
    let output = martite(&[s("lint"), s("--list-rules"), s("--json")]);
    assert!(output.status.success(), "{}", stderr(&output));
    let rules: Vec<Json> = serde_json::from_str(&stdout(&output)).expect("JSON array");
    assert_eq!(rules.len(), 32);
    let l001 = rules.iter().find(|rule| rule["id"] == "L001").unwrap();
    assert_eq!(l001["group"], "style");
    assert_eq!(l001["default_severity"], "warn");
    assert_eq!(l001["retired"], false);
}

#[test]
fn l022_under_rule_selection_is_reported_by_the_cli() {
    let dir = TempDir::new("l022");
    let script = dir.file(
        "main.mt",
        "fn main() {\n    // allow(L002)\n    var value = 1\n    print(value)\n}\n",
    );
    let selected = martite(&[
        s("lint"),
        s("--json"),
        s("--rule"),
        s("L005,L022"),
        script.as_os_str(),
    ]);
    assert!(selected.status.success(), "{}", stderr(&selected));
    assert!(
        !json_lines(&selected).iter().any(|d| d["code"] == "L022"),
        "{}",
        stdout(&selected)
    );

    let only_l022 = martite(&[
        s("lint"),
        s("--json"),
        s("--rule"),
        s("L022"),
        script.as_os_str(),
    ]);
    assert!(only_l022.status.success(), "{}", stderr(&only_l022));
    assert!(
        !json_lines(&only_l022).iter().any(|d| d["code"] == "L022"),
        "{}",
        stdout(&only_l022)
    );
}

#[test]
fn syntax_only_skips_resolved_and_typed_rules() {
    let dir = TempDir::new("syntax-only");
    let script = dir.file(
        "main.mt",
        "fn main() {\n    var value = 1\n    print(value)\n}\n",
    );
    let output = martite(&[
        s("lint"),
        s("--syntax-only"),
        s("--json"),
        script.as_os_str(),
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        !json_lines(&output).iter().any(|d| d["code"] == "L002"),
        "{}",
        stdout(&output)
    );

    let plugin = dir.file(
        "plugin.mt",
        "use app.api\non check() {\n    let text = api.maybe().expect(\"x\")\n    print(text)\n}\n",
    );
    let output = martite(&[
        s("lint"),
        s("--syntax-only"),
        s("--json"),
        plugin.as_os_str(),
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        !json_lines(&output).iter().any(|d| d["code"] == "L008"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn config_overrides_apply_to_cli_linting() {
    let dir = TempDir::new("config");
    dir.file(
        "martite.toml",
        "[lint]\npreset = \"recommended\"\n\n[[lint.override]]\npaths = [\"tests/**\"]\noff = [\"L005\"]\n",
    );
    dir.file(
        "tests/a.mt",
        "fn main() {\n    if true {\n        print(\"x\")\n    }\n}\n",
    );
    let output = martite_in(dir.path(), &[s("lint"), s("--json"), s("tests/a.mt")]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        !json_lines(&output).iter().any(|d| d["code"] == "L005"),
        "{}",
        stdout(&output)
    );
}
