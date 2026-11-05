use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(name: &str) -> Self {
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "martite-cli-{}-{}-{}",
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

fn martite_stdin(args: &[&std::ffi::OsStr], input: &str) -> Output {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_martite"))
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn martite");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait for martite")
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

#[test]
fn run_executes_a_script_and_passes_arguments() {
    let dir = TempDir::new("run");
    let script = dir.file(
        "main.mt",
        "use std.env\nfn main() { print(\"hello {env.args()}\") }\n",
    );
    let output = martite(&[s("run"), script.as_os_str(), s("alpha"), s("beta")]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(
        stdout(&output).contains("hello [\"alpha\", \"beta\"]"),
        "stdout: {}",
        stdout(&output)
    );
}

#[test]
fn run_reports_a_panic_with_location_and_exit_101() {
    let dir = TempDir::new("panic");
    let script = dir.file(
        "main.mt",
        "fn main() {\n    let xs = [1]\n    print(\"{xs[2]}\")\n}\n",
    );
    let output = martite(&[s("run"), script.as_os_str()]);
    assert_eq!(output.status.code(), Some(101));
    let error = stderr(&output);
    assert!(error.contains("panic:"), "stderr: {error}");
    assert!(error.contains("main.mt"), "stderr: {error}");
}

#[test]
fn check_reports_e201_and_json_on_stdout() {
    let dir = TempDir::new("check");
    let script = dir.file("main.mt", "fn main() {\n    let x: int = \"no\"\n}\n");
    let output = martite(&[s("check"), script.as_os_str()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("E201"),
        "stderr: {}",
        stderr(&output)
    );

    let json = martite(&[s("check"), s("--json"), script.as_os_str()]);
    assert_eq!(json.status.code(), Some(1));
    let text = stdout(&json);
    assert!(text.contains("\"code\":\"E201\""), "stdout: {text}");
    assert!(text.contains("\"line\":2"), "stdout: {text}");
}

#[test]
fn check_accepts_a_valid_file() {
    let dir = TempDir::new("ok");
    let script = dir.file("main.mt", "fn main() { print(\"ok\") }\n");
    let output = martite(&[s("check"), script.as_os_str()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
}

#[test]
fn test_runs_passing_and_failing_blocks() {
    let dir = TempDir::new("tests");
    let passing = dir.file(
        "passing.mt",
        "test \"works\" {\n    assert(1 + 1 == 2, \"math is broken\")\n}\n",
    );
    let output = martite(&[s("test"), passing.as_os_str()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("1/1 tests passed"));

    let failing = dir.file(
        "failing.mt",
        "test \"fails\" {\n    assert(false, \"boom\")\n}\n",
    );
    let output = martite(&[s("test"), failing.as_os_str()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stdout(&output).contains("FAILED"),
        "stdout: {}",
        stdout(&output)
    );
    assert!(
        stdout(&output).contains("boom"),
        "stdout: {}",
        stdout(&output)
    );
}

#[test]
fn check_loads_a_plugin_directory_with_an_interface() {
    let dir = TempDir::new("plugin");
    dir.file(
        "plugin.toml",
        r#"
[plugin]
name = "word-count"
version = "0.1.0"
lang = "1"
api = "1.0"
entry = "main.mt"

[permissions]
grant = ["app.editor"]
"#,
    );
    dir.file(
        "main.mt",
        "use app.editor\nuse app.editor.Doc\n\nstruct Stats { saves: int }\n\non init() -> Stats { Stats { saves: 0 } }\non saved(mut self, doc: Doc) -> Result[(), str] {\n    let _ = editor.text(doc)\n    self.saves += 1\n    Ok(())\n}\n",
    );
    let interface = dir.file(
        "app/editor.mti",
        "struct Doc\nfn text(doc: Doc) -> str\non saved(doc: Doc) -> Result[(), str]\n",
    );
    let output = martite(&[
        s("check"),
        s("--interface"),
        interface.as_os_str(),
        dir.path().as_os_str(),
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
}

#[test]
fn lint_lists_and_explains_rules() {
    let output = martite(&[s("lint"), s("--list-rules")]);
    assert!(output.status.success());
    assert!(stdout(&output).contains("L001"));

    let json = martite(&[s("lint"), s("--list-rules"), s("--json")]);
    assert!(json.status.success());
    assert!(stdout(&json).contains("\"id\":\"L001\""));

    let explain = martite(&[s("lint"), s("--explain"), s("L019")]);
    assert!(explain.status.success());
    assert!(stdout(&explain).contains("L019"));
}

#[test]
fn lint_syntax_only_reports_constant_conditions() {
    let dir = TempDir::new("lint-syntax");
    let script = dir.file("main.mt", "fn main() {\n    if true {}\n}\n");
    let output = martite(&[
        s("lint"),
        s("--syntax-only"),
        s("--deny-warnings"),
        script.as_os_str(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("L005"),
        "stderr: {}",
        stderr(&output)
    );
}

#[test]
fn lint_fix_applies_machine_fixes() {
    let dir = TempDir::new("lint-fix");
    let script = dir.file(
        "main.mt",
        "fn main() {\n    var value = 1\n    print(value)\n}\n",
    );
    let output = martite(&[s("lint"), s("--fix"), script.as_os_str()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = std::fs::read_to_string(&script).expect("read fixed file");
    assert!(text.contains("let value"), "fixed source: {text}");
    assert!(!text.contains("var value"), "fixed source: {text}");
}

#[test]
fn lint_deny_warnings_changes_the_exit_code() {
    let dir = TempDir::new("lint-deny");
    let script = dir.file(
        "main.mt",
        "fn main() {\n    if true {\n        print(\"x\")\n    }\n}\n",
    );
    let plain = martite(&[s("lint"), script.as_os_str()]);
    assert!(plain.status.success(), "stderr: {}", stderr(&plain));
    let denied = martite(&[s("lint"), s("--deny-warnings"), script.as_os_str()]);
    assert_eq!(denied.status.code(), Some(1));
}

#[test]
fn fmt_formats_in_place_and_check_reports_differences() {
    let dir = TempDir::new("fmt");
    let script = dir.file("main.mt", "fn main(){let x=[1,2,3]\nprint(\"{x}\")}");
    let checked = martite(&[s("fmt"), s("--check"), script.as_os_str()]);
    assert_eq!(checked.status.code(), Some(1));
    assert!(stdout(&checked).contains("--- a/"));
    assert!(stdout(&checked).contains("+++ b/"));
    assert_eq!(
        std::fs::read_to_string(&script).unwrap(),
        "fn main(){let x=[1,2,3]\nprint(\"{x}\")}"
    );

    let formatted = martite(&[s("fmt"), s("--verify"), script.as_os_str()]);
    assert!(formatted.status.success(), "stderr: {}", stderr(&formatted));
    let text = std::fs::read_to_string(&script).unwrap();
    assert!(text.contains("    let x = [1, 2, 3]"));

    let clean = martite(&[s("fmt"), s("--check"), script.as_os_str()]);
    assert!(clean.status.success(), "stderr: {}", stderr(&clean));
}

#[test]
fn fmt_stdin_writes_to_stdout() {
    let output = martite_stdin(&[s("fmt"), s("--stdin")], "fn main(){print(1)}");
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), "fn main() { print(1) }\n");

    let checked = martite_stdin(
        &[s("fmt"), s("--stdin"), s("--check")],
        "fn main(){print(1)}",
    );
    assert_eq!(checked.status.code(), Some(1));
    assert!(stdout(&checked).contains("-fn main(){print(1)}"));
}

#[test]
fn fmt_leaves_syntax_errors_untouched() {
    let dir = TempDir::new("fmt-bad");
    let source = "fn main( {\n";
    let script = dir.file("main.mt", source);
    let output = martite(&[s("fmt"), script.as_os_str()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("E003"),
        "stderr: {}",
        stderr(&output)
    );
    assert_eq!(std::fs::read_to_string(&script).unwrap(), source);
}

#[test]
fn fmt_does_not_change_suppression() {
    let dir = TempDir::new("fmt-suppress");
    let script = dir.file(
        "main.mt",
        "fn main() {\n    // allow(L005)\n    if true { print(\"x\") }\n}\n",
    );
    let before = martite(&[s("lint"), script.as_os_str()]);
    assert!(
        !stderr(&before).contains("L005"),
        "stderr: {}",
        stderr(&before)
    );
    let formatted = martite(&[s("fmt"), script.as_os_str()]);
    assert!(formatted.status.success(), "stderr: {}", stderr(&formatted));
    let after = martite(&[s("lint"), script.as_os_str()]);
    assert!(
        !stderr(&after).contains("L005"),
        "stderr: {}",
        stderr(&after)
    );
    let text = std::fs::read_to_string(&script).unwrap();
    assert!(text.contains("// allow(L005)\n    if true"), "{text}");
}

#[test]
fn fmt_is_stable_after_lint_fix() {
    let dir = TempDir::new("fmt-after-fix");
    let script = dir.file("main.mt", "fn main(){var value = 1\nprint(value)}\n");
    let fix = martite(&[s("lint"), s("--fix"), script.as_os_str()]);
    assert!(fix.status.success(), "stderr: {}", stderr(&fix));
    let formatted = martite(&[s("fmt"), script.as_os_str()]);
    assert!(formatted.status.success(), "stderr: {}", stderr(&formatted));
    let checked = martite(&[s("fmt"), s("--check"), script.as_os_str()]);
    assert!(checked.status.success(), "stderr: {}", stderr(&checked));
}

#[test]
fn fmt_uses_martite_toml_settings_and_walks_projects() {
    let dir = TempDir::new("fmt-config");
    dir.file(
        "martite.toml",
        "[fmt]\nmax_width = 40\nindent = 2\nend_of_line = \"crlf\"\n",
    );
    let script = dir.file("main.mt", "fn main(){let x=[1,2,3]}\n");
    let interface = dir.file("api.mti", "struct Doc\nfn text(d: Doc)->str\n");
    let output = martite_in(dir.path(), &[s("fmt")]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = std::fs::read_to_string(&script).unwrap();
    assert!(text.contains("\r\n"), "expected CRLF: {text:?}");
    assert!(
        text.contains("  let x"),
        "expected 2-space indent: {text:?}"
    );
    let mti = std::fs::read_to_string(&interface).unwrap();
    assert_eq!(mti, "struct Doc\r\n\r\nfn text(d: Doc) -> str\r\n");
}
