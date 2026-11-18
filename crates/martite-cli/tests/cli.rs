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
fn run_reports_load_errors_with_source_location() {
    let dir = TempDir::new("run-load-error");
    let script = dir.file("main.mt", "fn main() {\n    let x: int = \"no\"\n}\n");
    let output = martite(&[s("run"), script.as_os_str()]);
    assert_eq!(output.status.code(), Some(2));
    let error = stderr(&output);
    assert!(error.contains("E201"), "stderr: {error}");
    assert!(error.contains("main.mt:2:"), "stderr: {error}");

    let json = martite(&[s("run"), s("--json"), script.as_os_str()]);
    assert_eq!(json.status.code(), Some(2));
    let text = stdout(&json);
    assert!(text.contains("\"file\":\"main.mt\""), "stdout: {text}");
    assert!(text.contains("\"line\":2"), "stdout: {text}");
    assert!(text.contains("\"span\":"), "stdout: {text}");
}

#[test]
fn check_accepts_a_valid_file() {
    let dir = TempDir::new("ok");
    let script = dir.file("main.mt", "fn main() { print(\"ok\") }\n");
    let output = martite(&[s("check"), script.as_os_str()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
}

#[test]
fn check_rejects_duplicate_bindings_in_one_pattern() {
    let dir = TempDir::new("check-duplicate");
    let script = dir.file(
        "main.mt",
        "fn main() {\n    let (x, x) = (1, 2)\n    print(\"{x}\")\n}\n",
    );
    let output = martite(&[s("check"), script.as_os_str()]);
    assert_eq!(output.status.code(), Some(1));
    let error = stderr(&output);
    assert!(error.contains("E303"), "stderr: {error}");
}

#[test]
fn check_rejects_range_assignment_before_lowering() {
    let dir = TempDir::new("check-range-assign");
    let script = dir.file(
        "main.mt",
        "fn main() {\n    var xs = [1, 2]\n    xs[0..1] = []\n    print(\"{xs.len()}\")\n}\n",
    );
    let checked = martite(&[s("check"), script.as_os_str()]);
    assert_eq!(checked.status.code(), Some(1));
    assert!(
        stderr(&checked).contains("E201"),
        "stderr: {}",
        stderr(&checked)
    );

    let run = martite(&[s("run"), script.as_os_str()]);
    let error = stderr(&run);
    assert!(error.contains("E201"), "stderr: {error}");
    assert!(!error.contains("E508"), "stderr: {error}");

    let string = dir.file(
        "string.mt",
        "fn main() {\n    var s = \"ab\"\n    s[0..1] = \"x\"\n}\n",
    );
    let checked = martite(&[s("check"), string.as_os_str()]);
    assert_eq!(checked.status.code(), Some(1));
    assert!(
        stderr(&checked).contains("E401"),
        "stderr: {}",
        stderr(&checked)
    );

    let run = martite(&[s("run"), string.as_os_str()]);
    let error = stderr(&run);
    assert!(error.contains("E401"), "stderr: {error}");
    assert!(!error.contains("E508"), "stderr: {error}");

    let mutable = dir.file(
        "mutable.mt",
        "fn bump(mut xs: list[int]) { xs.push(1) }\nfn main() {\n    var xs = [1, 2]\n    bump(mut xs[0..1])\n}\n",
    );
    let checked = martite(&[s("check"), mutable.as_os_str()]);
    assert_eq!(checked.status.code(), Some(1));
    assert!(
        stderr(&checked).contains("E201"),
        "stderr: {}",
        stderr(&checked)
    );

    let run = martite(&[s("run"), mutable.as_os_str()]);
    let error = stderr(&run);
    assert!(error.contains("E201"), "stderr: {error}");
    assert!(!error.contains("E508"), "stderr: {error}");
}

#[test]
fn check_rejects_private_methods_from_another_module() {
    let dir = TempDir::new("check-private-method");
    dir.file(
        "a.mt",
        "pub struct Box { n: int }\nimpl Box {\n    fn secret(self) -> int { self.n }\n    pub fn make() -> Box { Box { n: 1 } }\n}\n",
    );
    let script = dir.file(
        "main.mt",
        "use a.Box\nfn main() {\n    let b = Box.make()\n    let n = b.secret()\n    print(\"{n}\")\n}\n",
    );
    let output = martite(&[s("check"), script.as_os_str()]);
    assert_eq!(output.status.code(), Some(1));
    let error = stderr(&output);
    assert!(error.contains("E106"), "stderr: {error}");
    assert!(error.contains("private"), "stderr: {error}");
}

#[cfg(windows)]
#[test]
fn case_insensitive_filesystem_keeps_type_imports_as_items() {
    let dir = TempDir::new("check-type-import-case");
    let script = dir.file("main.mt", "use a.Point\nfn main() { }\n");
    dir.file("a.mt", "pub struct Point { x: int }\n");
    dir.file("a/point.mt", "pub struct Broken {\n");

    let output = martite(&[s("check"), script.as_os_str()]);
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        stderr(&output),
        stdout(&output)
    );
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
fn test_without_ambient_host_rejects_std_modules() {
    let dir = TempDir::new("test-no-std");
    let script = dir.file(
        "main.mt",
        "use std.io\n\ntest \"reads a file\" {\n    let missing = io.read_file(\"definitely-not-a-real-file.txt\")\n    assert(missing.is_err(), \"expected a read error\")\n}\n",
    );
    let output = martite(&[s("test"), script.as_os_str()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("E106"),
        "stderr: {}",
        stderr(&output)
    );
}

#[test]
fn test_runs_std_host_calls_in_scripts_with_ambient_host() {
    let dir = TempDir::new("test-std");
    let script = dir.file(
        "main.mt",
        "use std.env\nuse std.io\n\ntest \"standalone host is registered\" {\n    assert(env.args().len() == 0, \"no script arguments\")\n    let missing = io.read_file(\"definitely-not-a-real-file.txt\")\n    assert(missing.is_err(), \"expected a read error\")\n}\n",
    );
    let output = martite(&[s("test"), s("--ambient-host"), script.as_os_str()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(
        stdout(&output).contains("1/1 tests passed"),
        "stdout: {}",
        stdout(&output)
    );
}

#[test]
fn check_accepts_std_host_calls_in_scripts() {
    let dir = TempDir::new("check-std");
    let script = dir.file(
        "main.mt",
        "use std.io\n\nfn main() {\n    match io.read_file(\"x\") {\n        Ok(text) => print(text),\n        Err(message) => print(message),\n    }\n}\n",
    );
    let output = martite(&[s("check"), script.as_os_str()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
}

#[test]
fn test_reports_load_errors_with_source_location() {
    let dir = TempDir::new("test-load-error");
    let script = dir.file("main.mt", "test \"broken\" {\n    let x: int = \"no\"\n}\n");
    let output = martite(&[s("test"), script.as_os_str()]);
    assert_eq!(output.status.code(), Some(1));
    let error = stderr(&output);
    assert!(error.contains("E201"), "stderr: {error}");
    assert!(error.contains("main.mt:2:"), "stderr: {error}");
}

#[test]
fn test_reports_a_malformed_manifest_as_e508() {
    let dir = TempDir::new("test-manifest");
    dir.file("plugin.toml", "[plugin\nname = \"x\"\n");
    let output = martite(&[s("test"), dir.path().as_os_str()]);
    assert_eq!(output.status.code(), Some(1));
    let error = stderr(&output);
    assert!(error.contains("E508"), "stderr: {error}");
}

#[test]
fn test_loads_plugin_directories_without_host_impls() {
    let dir = TempDir::new("test-plugin");
    dir.file(
        "plugin.toml",
        r#"
[plugin]
name = "test-plugin"
version = "0.1.0"
lang = "1"
api = "1.0"
entry = "main.mt"

[permissions]
grant = ["app.api"]
"#,
    );
    dir.file(
        "main.mt",
        "use app.api\n\ntest \"offline\" {\n    assert(1 + 1 == 2, \"math\")\n}\n\ntest \"needs a double\" {\n    let _ = api.value()\n}\n",
    );
    let interface = dir.file("app/api.mti", "fn value() -> int\n");
    let output = martite(&[
        s("test"),
        s("--interface"),
        interface.as_os_str(),
        dir.path().as_os_str(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    let text = stdout(&output);
    assert!(text.contains("test offline ... ok"), "stdout: {text}");
    assert!(
        text.contains("test needs a double ... FAILED"),
        "stdout: {text}"
    );
    assert!(text.contains("no test double"), "stdout: {text}");
    assert!(text.contains("1/2 tests passed"), "stdout: {text}");
}

#[test]
fn run_registers_cli_interfaces() {
    let dir = TempDir::new("run-interface");
    let interface = dir.file("api.mti", "const LIMIT: int = 5\n");
    let script = dir.file(
        "main.mt",
        "use api.LIMIT\n\nfn main() {\n    print(\"{LIMIT}\")\n}\n",
    );
    let output = martite(&[
        s("run"),
        s("--interface"),
        interface.as_os_str(),
        script.as_os_str(),
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains('5'), "stdout: {}", stdout(&output));
}

#[test]
fn common_argument_parsing_stays_per_command() {
    let dir = TempDir::new("args");
    let script = dir.file("main.mt", "fn main() {}\n");

    let no_optimize = martite(&[s("check"), s("--no-optimize"), script.as_os_str()]);
    assert_eq!(no_optimize.status.code(), Some(2));

    let run_optimize = martite(&[s("run"), s("--optimize"), script.as_os_str()]);
    assert_eq!(run_optimize.status.code(), Some(2));

    let test_optimize = martite(&[s("test"), s("--optimize"), script.as_os_str()]);
    assert_eq!(test_optimize.status.code(), Some(2));

    let lower_dump = martite(&[s("lower"), s("--dump-ir"), script.as_os_str()]);
    assert_eq!(lower_dump.status.code(), Some(2));
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
fn plugin_entry_with_dots_loads_that_file() {
    let dir = TempDir::new("entry-dots");
    dir.file(
        "plugin.toml",
        "[plugin]\nname = \"entry-dots\"\nversion = \"0.1.0\"\nlang = \"1\"\napi = \"1.0\"\nentry = \"main.v1.mt\"\n\n[permissions]\ngrant = [\"app.editor\"]\n",
    );
    dir.file(
        "main.v1.mt",
        "use app.editor\n\non query(_text: str) -> () { }\n",
    );
    dir.file("main/v1.mt", "on query(\n");
    let interface = dir.file("app/editor.mti", "on query(text: str) -> ()\n");

    let output = martite(&[
        s("check"),
        s("--interface"),
        interface.as_os_str(),
        dir.path().as_os_str(),
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
}

#[test]
fn plugin_owned_workspace_cannot_supply_interfaces() {
    let dir = TempDir::new("plugin-workspace");
    let plugin = dir.path().join("plugin");
    dir.file(
        "plugin/martite.toml",
        "[workspace]\ninterface-packages = [\"ifaces\"]\n",
    );
    dir.file(
        "plugin/plugin.toml",
        "[plugin]\nname = \"plugin-workspace\"\nversion = \"0.1.0\"\nlang = \"1\"\napi = \"1.0\"\nentry = \"main.mt\"\n\n[permissions]\ngrant = [\"app.editor\"]\n",
    );
    dir.file(
        "plugin/main.mt",
        "use app.editor\n\non query(_text: str) -> int { editor.text(1) }\n",
    );
    dir.file(
        "plugin/ifaces/interface.toml",
        "[interface]\nname = \"app\"\n",
    );
    dir.file("plugin/ifaces/editor.mti", "fn text(doc: int) -> int\n");

    let output = martite(&[s("check"), plugin.as_os_str()]);
    assert_eq!(output.status.code(), Some(1));
    let error = stderr(&output);
    assert!(
        error.contains("E508") || error.contains("E106"),
        "stderr: {error}"
    );
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
fn check_caps_diagnostic_floods() {
    let dir = TempDir::new("diag-cap");
    let script = dir.file("main.mt", &"@".repeat(40_000));
    let output = martite(&[s("check"), script.as_os_str()]);
    assert_eq!(output.status.code(), Some(1));
    let error = stderr(&output);
    assert!(
        error.contains("more diagnostics suppressed"),
        "stderr: {error}"
    );
    assert!(
        error.matches("E001").count() <= martite_base::MAX_DIAGNOSTICS,
        "too many diagnostics rendered: {error}"
    );
}

#[test]
fn fmt_and_syntax_only_lint_reject_oversized_sources() {
    let dir = TempDir::new("oversized-tool");
    let limit = martite_base::LoadLimits::default().file_size;
    let script = dir.file("main.mt", &"x".repeat(limit + 1));

    let formatted = martite(&[s("fmt"), s("--check"), script.as_os_str()]);
    assert_eq!(formatted.status.code(), Some(2));
    assert!(
        stderr(&formatted).contains("exceeds"),
        "stderr: {}",
        stderr(&formatted)
    );

    let linted = martite(&[s("lint"), s("--syntax-only"), script.as_os_str()]);
    assert_eq!(linted.status.code(), Some(2));
    assert!(
        stderr(&linted).contains("exceeds"),
        "stderr: {}",
        stderr(&linted)
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
fn fmt_stdin_verify_preserves_field_initializers_and_choice_comments() {
    let field = "struct P { x: int }\nfn main() {\n    let x = 1\n    let p = P { x // note\n    : 2 }\n    print(\"{p.x}\")\n}\n";
    let formatted = martite_stdin(&[s("fmt"), s("--stdin"), s("--verify")], field);
    assert!(formatted.status.success(), "stderr: {}", stderr(&formatted));
    let text = stdout(&formatted);
    assert!(text.contains("// note"), "{text}");
    assert!(text.contains("x: 2"), "{text}");

    let dir = TempDir::new("fmt-trivia");
    let script = dir.file("main.mt", &text);
    let run = martite(&[s("run"), script.as_os_str()]);
    assert!(run.status.success(), "stderr: {}", stderr(&run));
    assert_eq!(stdout(&run).trim(), "2");

    let if_source = "fn main() {\n    let x = true\n    if x { print(\"a\") } // stop here\n}\n";
    let call = "fn main() {\n    let xs = [1, 2]\n    let ys = xs.map(fn(x) { x }) // after\n    print(\"{ys.len()}\")\n}\n";
    for source in [if_source, call] {
        let output = martite_stdin(&[s("fmt"), s("--stdin"), s("--verify")], source);
        assert!(output.status.success(), "stderr: {}", stderr(&output));
        let text = stdout(&output);
        assert_eq!(
            text.matches("// stop here").count() + text.matches("// after").count(),
            1,
            "{text}"
        );
    }
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

fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

fn lower_golden_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/lower")
}

const LOWER_EXAMPLES: &[&str] = &["hello", "env", "tests"];

#[test]
fn lower_prints_deterministic_ir() {
    let dir = TempDir::new("lower");
    let script = dir.file("main.mt", "fn main() { print(1 + 2) }\n");
    let first = martite(&[s("lower"), script.as_os_str()]);
    assert!(first.status.success(), "stderr: {}", stderr(&first));
    let text = stdout(&first);
    assert!(text.starts_with("ir-format 1\n"), "stdout: {text}");
    assert!(text.contains("\"main\""), "stdout: {text}");
    assert!(text.contains("span=src#"), "stdout: {text}");
    assert!(stderr(&first).is_empty(), "stderr: {}", stderr(&first));

    let second = martite(&[s("lower"), script.as_os_str()]);
    assert!(second.status.success(), "stderr: {}", stderr(&second));
    assert_eq!(text, stdout(&second));
}

#[test]
fn lower_goldens_match_examples() {
    for name in LOWER_EXAMPLES {
        let example = examples_dir().join(format!("{name}.mt"));
        let output = martite(&[s("lower"), example.as_os_str()]);
        assert!(
            output.status.success(),
            "{name}: stderr: {}",
            stderr(&output)
        );
        let path = lower_golden_root().join(format!("{name}.ir"));
        let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!(
                "missing golden {}; run:\n  cargo test -p martite-cli --test cli -- --ignored update_lower_goldens",
                path.display()
            )
        });
        assert_eq!(stdout(&output), expected, "golden mismatch for {name}");
    }
}

#[test]
#[ignore = "regenerates the checked-in CLI lower goldens"]
fn update_lower_goldens() {
    let root = lower_golden_root();
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create golden directory");
    for name in LOWER_EXAMPLES {
        let example = examples_dir().join(format!("{name}.mt"));
        let output = martite(&[s("lower"), example.as_os_str()]);
        assert!(
            output.status.success(),
            "{name}: stderr: {}",
            stderr(&output)
        );
        std::fs::write(root.join(format!("{name}.ir")), &output.stdout)
            .expect("write lower golden");
    }
}

#[test]
fn lower_reports_load_diagnostics_without_dumping_ir() {
    let output = martite(&[s("lower"), examples_dir().join("bad.mt").as_os_str()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty(), "stdout: {}", stdout(&output));
    assert!(
        stderr(&output).contains("E201"),
        "stderr: {}",
        stderr(&output)
    );
}

#[test]
fn lower_usage_errors_exit_two() {
    let missing = martite(&[s("lower")]);
    assert_eq!(missing.status.code(), Some(2));

    let not_found = martite(&[s("lower"), s("does-not-exist.mt")]);
    assert_eq!(not_found.status.code(), Some(2));

    let dir = TempDir::new("lower-usage");
    let first = dir.file("one.mt", "fn main() {}\n");
    let second = dir.file("two.mt", "fn main() {}\n");
    let two = martite(&[s("lower"), first.as_os_str(), second.as_os_str()]);
    assert_eq!(two.status.code(), Some(2));

    let irrelevant = martite(&[s("lower"), s("--fuel"), s("10"), first.as_os_str()]);
    assert_eq!(irrelevant.status.code(), Some(2));
}

#[test]
fn dump_ir_keeps_run_output_unchanged() {
    let dir = TempDir::new("dump-run");
    let script = dir.file("main.mt", "fn main() { print(\"hello\") }\n");
    let plain = martite(&[s("run"), script.as_os_str()]);
    assert!(plain.status.success(), "stderr: {}", stderr(&plain));

    let dumped = martite(&[s("run"), s("--dump-ir"), script.as_os_str()]);
    assert!(dumped.status.success(), "stderr: {}", stderr(&dumped));
    assert_eq!(stdout(&plain), stdout(&dumped));
    let error = stderr(&dumped);
    assert!(
        error.contains("; IR before optimization"),
        "stderr: {error}"
    );
    assert!(error.contains("; IR after optimization"), "stderr: {error}");
    assert_eq!(error.matches("ir-format 1").count(), 2, "stderr: {error}");

    let before = martite(&[s("run"), s("--dump-ir=before"), script.as_os_str()]);
    assert!(before.status.success(), "stderr: {}", stderr(&before));
    assert_eq!(stdout(&plain), stdout(&before));
    let error = stderr(&before);
    assert!(
        error.contains("; IR before optimization"),
        "stderr: {error}"
    );
    assert!(
        !error.contains("; IR after optimization"),
        "stderr: {error}"
    );
    assert_eq!(error.matches("ir-format 1").count(), 1, "stderr: {error}");
}

#[test]
fn dump_ir_keeps_test_output_unchanged() {
    let dir = TempDir::new("dump-test");
    let script = dir.file(
        "main.mt",
        "test \"works\" {\n    assert(1 + 1 == 2, \"math is broken\")\n}\n",
    );
    let plain = martite(&[s("test"), script.as_os_str()]);
    assert!(plain.status.success(), "stderr: {}", stderr(&plain));

    let dumped = martite(&[s("test"), s("--dump-ir=after"), script.as_os_str()]);
    assert!(dumped.status.success(), "stderr: {}", stderr(&dumped));
    assert_eq!(stdout(&plain), stdout(&dumped));
    let error = stderr(&dumped);
    assert!(error.contains("; IR after optimization"), "stderr: {error}");
    assert!(
        !error.contains("; IR before optimization"),
        "stderr: {error}"
    );
    assert_eq!(error.matches("ir-format 1").count(), 1, "stderr: {error}");
}

#[test]
fn lower_optimize_golden_matches() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/optimize/const_fold.mt");
    let output = martite(&[s("lower"), s("--optimize"), fixture.as_os_str()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/optimize/const_fold.ir");
    let expected = std::fs::read_to_string(&golden)
        .unwrap_or_else(|_| panic!("missing golden {}", golden.display()));
    assert_eq!(stdout(&output), expected);
}

#[test]
fn lower_optimize_prints_carried_charges() {
    let dir = TempDir::new("lower-optimize");
    let script = dir.file("main.mt", "fn main() { let value = 1 + 2 }\n");

    let lowered = martite(&[s("lower"), script.as_os_str()]);
    assert!(lowered.status.success(), "stderr: {}", stderr(&lowered));
    let lowered_text = stdout(&lowered);
    assert!(lowered_text.contains(" add "), "stdout: {lowered_text}");
    assert!(
        !lowered_text.contains("charge units="),
        "stdout: {lowered_text}"
    );

    let optimized = martite(&[s("lower"), s("--optimize"), script.as_os_str()]);
    assert!(optimized.status.success(), "stderr: {}", stderr(&optimized));
    let optimized_text = stdout(&optimized);
    assert!(
        optimized_text.contains("charge units=1"),
        "stdout: {optimized_text}"
    );
    assert!(
        optimized_text.contains("value=3"),
        "stdout: {optimized_text}"
    );
    assert!(
        !optimized_text.contains(" add "),
        "stdout: {optimized_text}"
    );
}

#[test]
fn run_optimizes_by_default_and_no_optimize_opts_out() {
    let dir = TempDir::new("run-optimize");
    let script = dir.file("main.mt", "fn main() { print(1 + 2) }\n");

    let optimized = martite(&[s("run"), s("--dump-ir=after"), script.as_os_str()]);
    assert!(optimized.status.success(), "stderr: {}", stderr(&optimized));
    let error = stderr(&optimized);
    assert!(error.contains("charge units=1"), "stderr: {error}");
    assert!(!error.contains(" add "), "stderr: {error}");

    let plain = martite(&[s("run"), s("--no-optimize"), script.as_os_str()]);
    assert!(plain.status.success(), "stderr: {}", stderr(&plain));
    assert_eq!(stdout(&plain), "3\n");
    let lowered = martite(&[
        s("run"),
        s("--no-optimize"),
        s("--dump-ir=after"),
        script.as_os_str(),
    ]);
    assert!(lowered.status.success(), "stderr: {}", stderr(&lowered));
    let error = stderr(&lowered);
    assert!(!error.contains("charge units="), "stderr: {error}");
    assert!(error.contains(" add "), "stderr: {error}");
    assert_eq!(stdout(&optimized), stdout(&plain));
}

#[test]
fn dump_ir_rejects_unknown_stages_and_other_commands() {
    let dir = TempDir::new("dump-usage");
    let script = dir.file("main.mt", "fn main() {}\n");

    let unknown = martite(&[s("run"), s("--dump-ir=sideways"), script.as_os_str()]);
    assert_eq!(unknown.status.code(), Some(2));

    let check = martite(&[s("check"), s("--dump-ir"), script.as_os_str()]);
    assert_eq!(check.status.code(), Some(2));
    assert!(
        stderr(&check).contains("--dump-ir"),
        "stderr: {}",
        stderr(&check)
    );
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

fn workspace_fixture(dir: &TempDir) {
    dir.file(
        "martite.toml",
        "[workspace]\nmembers = [\"plugins/*\"]\ninterface-packages = [\"interfaces/nvlaunch\"]\n",
    );
    dir.file(
        "interfaces/nvlaunch/interface.toml",
        "[interface]\nname = \"nvlaunch\"\n",
    );
    dir.file(
        "interfaces/nvlaunch/launcher.mti",
        "struct Item { id: str, title: str, detail: str, glyph: str }\n\non query(text: str) -> list[Item]\non activate(id: str, text: str) -> Result[(), str]\n",
    );
    for name in ["calculator", "github"] {
        dir.file(
            &format!("plugins/{name}/plugin.toml"),
            &format!(
                "[plugin]\nname = \"{name}\"\nversion = \"0.1.0\"\nlang = \"1\"\napi = \"1.0\"\nentry = \"main.mt\"\n\n[permissions]\ngrant = [\"nvlaunch.launcher\"]\n"
            ),
        );
        dir.file(
            &format!("plugins/{name}/main.mt"),
            "use nvlaunch.launcher\nuse nvlaunch.launcher.Item\n\non query(_text: str) -> list[Item] { [] }\n\non activate(_id: str, _text: str) -> Result[(), str] { Ok(()) }\n\ntest \"works\" { assert(1 + 1 == 2, \"math\") }\n",
        );
    }
}

#[test]
fn workspace_check_uses_shared_interfaces_for_every_member() {
    let dir = TempDir::new("workspace-check");
    workspace_fixture(&dir);

    let output = martite_in(dir.path(), &[s("check")]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let output = martite_in(dir.path(), &[s("check"), s("plugins/calculator")]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
}

#[test]
fn workspace_test_runs_every_member_and_aggregates() {
    let dir = TempDir::new("workspace-test");
    workspace_fixture(&dir);

    let output = martite_in(dir.path(), &[s("test")]);
    assert!(output.status.success(), "stdout: {}", stdout(&output));
    let text = stdout(&output);
    assert!(
        text.contains("calculator: 1/1 tests passed"),
        "stdout: {text}"
    );
    assert!(text.contains("github: 1/1 tests passed"), "stdout: {text}");
    assert!(text.contains("2/2 tests passed"), "stdout: {text}");
}

#[test]
fn workspace_lint_treats_members_as_plugins() {
    let dir = TempDir::new("workspace-lint");
    workspace_fixture(&dir);

    let output = martite_in(dir.path(), &[s("lint")]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
}

#[test]
fn workspace_diagnostics_are_prefixed_with_the_member() {
    let dir = TempDir::new("workspace-prefix");
    workspace_fixture(&dir);
    dir.file(
        "plugins/github/main.mt",
        "use nvlaunch.launcher\nuse nvlaunch.launcher.Item\n\non query(_text: str) -> list[Item] { missing }\n\non activate(_id: str, _text: str) -> Result[(), str] { Ok(()) }\n",
    );

    let output = martite_in(dir.path(), &[s("check")]);
    assert_eq!(output.status.code(), Some(1));
    let error = stderr(&output);
    assert!(error.contains("plugins/github/main.mt"), "stderr: {error}");
    assert!(
        !error.contains("plugins/calculator/main.mt"),
        "stderr: {error}"
    );
}
