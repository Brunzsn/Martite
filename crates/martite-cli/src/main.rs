use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use martite::{Engine, HostSignature, HostType, LANG_EDITION, LoadError, Plugin, VerifyMode};
use martite_base::{
    Applicability, Code, Diagnostic, FileSystemProvider, LoadLimits, Severity, SourceMap,
    SourceProvider, normalize_source,
};
use martite_check::CheckedProgram;
use martite_config::{Manifest, ToolConfig, parse_manifest, parse_tool_config};
use martite_driver::{
    LoadOptions, LoadedProgram, LowerOptions, Workspace, discover_workspace,
    discover_workspace_root, load, lower_with,
};
use martite_ir::{IrProgram, Printer};
use martite_lint::{LintInputs, ManifestInput, SettingsResolver, normalize_path, sort_diagnostics};
use martite_resolve::InterfaceSource;
use martite_runtime::{ExecLimits, Trap, Value};
use martite_syntax::{parse_interface, parse_source};

const USAGE: &str = "\
Martite language toolchain

Usage:
    martite <command> [arguments]

Commands:
    run <file>            Run a script
    check [options] <path> Parse and type-check
    lower [options] <file> Lower a script and print the resolved IR
    lint [options] <path> Lint a project
    test [options] <path> Run test blocks
    fmt [options] [paths] Format sources
    help                  Show this help
    version               Show the toolchain version

Options:
    --json                Emit JSON diagnostics
    --interface <file>    Add a host interface (.mti)
    --fuel <n>            Fuel for `run` and `test`
    --memory <n>          Memory limit in bytes for `run` and `test`
    --depth <n>           Call-depth limit for `run` and `test`
    --no-optimize         Skip optimizer passes for `run` and `test`
    --dump-ir[=stage]     Dump IR for `run`/`test`: before, after, both

Lower options:
    --optimize            Print the optimized IR instead of the lowered IR

Fmt options:
    --check               Print a unified diff; do not write
    --verify              Re-parse output and compare ASTs
    --stdin               Read source from stdin and write to stdout

Lint options:
    --fix                 Apply machine-applicable fixes
    --fix --suggested     Also apply fixes that need review
    --deny-warnings       Exit 1 when warnings are reported
    --syntax-only         Run only syntax-tier rules
    --rule L001,L002      Run only the selected rules
    --list-rules          List every rule
    --explain L019        Explain one rule
";

/// `run`, `test` and `lower` verify lowered and optimized IR by default.
const CLI_VERIFICATION: VerifyMode = VerifyMode::Always;

const STD_IO: &str = r#"fn read_line() -> Result[str?, str]
fn read_file(path: str) -> Result[str, str]
fn write_file(path: str, text: str) -> Result[(), str]
"#;

const STD_ENV: &str = r#"fn args() -> list[str]
fn get(name: str) -> str?
"#;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let rest = &args[1..];
    match command.as_str() {
        "run" => cmd_run(rest),
        "check" => cmd_check(rest),
        "lower" => cmd_lower(rest),
        "lint" => cmd_lint(rest),
        "test" => cmd_test(rest),
        "fmt" => cmd_fmt(rest),
        "help" | "-h" | "--help" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        "version" | "-V" | "--version" => {
            println!(
                "martite {} (lang edition {LANG_EDITION})",
                env!("CARGO_PKG_VERSION")
            );
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("error: unknown command `{other}`\n");
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum DumpStage {
    Before,
    After,
    #[default]
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Run,
    Check,
    Test,
    Lower,
}

#[derive(Default)]
struct CliArgs {
    json: bool,
    interfaces: Vec<PathBuf>,
    fuel: Option<u64>,
    memory: Option<u64>,
    depth: Option<u32>,
    dump_ir: Option<DumpStage>,
    no_optimize: bool,
    optimize: bool,
    path: Option<String>,
    /// Arguments after the script path, passed through to `std.env.args()`.
    script_args: Vec<String>,
}

fn parse_args(args: &[String], command: Command) -> Result<CliArgs, String> {
    use lexopt::prelude::*;

    let allow_trailing = command == Command::Run;
    let mut out = CliArgs::default();
    let mut parser = lexopt::Parser::from_args(args.iter().cloned());
    while let Some(arg) = parser.next().map_err(|error| error.to_string())? {
        match arg {
            Long("json") => out.json = true,
            Long("interface") => {
                let path = parser
                    .value()
                    .map_err(|error| error.to_string())?
                    .string()
                    .map_err(|error| error.to_string())?;
                out.interfaces.push(PathBuf::from(path));
            }
            Long("fuel") => {
                out.fuel = Some(
                    parser
                        .value()
                        .map_err(|error| error.to_string())?
                        .string()
                        .map_err(|error| error.to_string())?
                        .parse()
                        .map_err(|_| "--fuel requires a number")?,
                );
            }
            Long("memory") => {
                out.memory = Some(
                    parser
                        .value()
                        .map_err(|error| error.to_string())?
                        .string()
                        .map_err(|error| error.to_string())?
                        .parse()
                        .map_err(|_| "--memory requires a number")?,
                );
            }
            Long("depth") => {
                out.depth = Some(
                    parser
                        .value()
                        .map_err(|error| error.to_string())?
                        .string()
                        .map_err(|error| error.to_string())?
                        .parse()
                        .map_err(|_| "--depth requires a number")?,
                );
            }
            Long("no-optimize") => out.no_optimize = true,
            Long("optimize") => out.optimize = true,
            Long("dump-ir") => {
                out.dump_ir = Some(match parser.optional_value() {
                    None => DumpStage::Both,
                    Some(value) => {
                        match value.string().map_err(|error| error.to_string())?.as_str() {
                            "before" => DumpStage::Before,
                            "after" => DumpStage::After,
                            "both" => DumpStage::Both,
                            _ => {
                                return Err(
                                    "--dump-ir requires `before`, `after`, or `both`".into()
                                );
                            }
                        }
                    }
                });
            }
            Long("fix") => {
                return Err("--fix is only supported by `martite lint`".into());
            }
            Value(value) => {
                let value = value.string().map_err(|error| error.to_string())?;
                if out.path.is_none() {
                    out.path = Some(value);
                } else if !allow_trailing {
                    return Err("expected a single path".into());
                } else {
                    out.script_args.push(value);
                }
                if allow_trailing && out.path.is_some() {
                    for value in parser.raw_args().map_err(|error| error.to_string())? {
                        out.script_args
                            .push(value.string().map_err(|error| error.to_string())?);
                    }
                    break;
                }
            }
            _ => return Err(arg.unexpected().to_string()),
        }
    }
    match command {
        Command::Check => {
            if out.dump_ir.is_some() {
                return Err(
                    "--dump-ir is only supported by `martite run` and `martite test`".into(),
                );
            }
            if out.no_optimize {
                return Err(
                    "--no-optimize is only supported by `martite run` and `martite test`".into(),
                );
            }
            if out.optimize {
                return Err("unexpected option '--optimize'".into());
            }
        }
        Command::Run | Command::Test => {
            if out.optimize {
                return Err("unexpected option '--optimize'".into());
            }
        }
        Command::Lower => {
            if out.fuel.is_some() || out.memory.is_some() || out.depth.is_some() {
                return Err("`lower` does not accept --fuel, --memory, or --depth".into());
            }
            if out.dump_ir.is_some() {
                return Err("`lower` does not accept --dump-ir".into());
            }
            if out.no_optimize {
                return Err("`lower` does not accept --no-optimize".into());
            }
        }
    }
    Ok(out)
}

#[derive(Default)]
struct LintArgs {
    json: bool,
    interfaces: Vec<PathBuf>,
    paths: Vec<String>,
    fix: bool,
    suggested: bool,
    deny_warnings: bool,
    syntax_only: bool,
    rules: Option<HashSet<Code>>,
    list_rules: bool,
    explain: Option<String>,
}

fn parse_lint_args(args: &[String]) -> Result<LintArgs, String> {
    use lexopt::prelude::*;

    let mut out = LintArgs::default();
    let mut parser = lexopt::Parser::from_args(args.iter().cloned());
    while let Some(arg) = parser.next().map_err(|error| error.to_string())? {
        match arg {
            Long("json") => out.json = true,
            Long("interface") => {
                let path = parser
                    .value()
                    .map_err(|error| error.to_string())?
                    .string()
                    .map_err(|error| error.to_string())?;
                out.interfaces.push(PathBuf::from(path));
            }
            Long("fix") => out.fix = true,
            Long("suggested") => out.suggested = true,
            Long("deny-warnings") => out.deny_warnings = true,
            Long("syntax-only") => out.syntax_only = true,
            Long("list-rules") => out.list_rules = true,
            Long("rule") => {
                let value = parser
                    .value()
                    .map_err(|error| error.to_string())?
                    .string()
                    .map_err(|error| error.to_string())?;
                out.rules = Some(martite_lint::parse_selection(&value)?);
            }
            Long("explain") => {
                out.explain = Some(
                    parser
                        .value()
                        .map_err(|error| error.to_string())?
                        .string()
                        .map_err(|error| error.to_string())?,
                );
            }
            Value(value) => out
                .paths
                .push(value.string().map_err(|error| error.to_string())?),
            _ => return Err(arg.unexpected().to_string()),
        }
    }
    if out.fix && out.syntax_only {
        return Err("--fix and --syntax-only are mutually exclusive".into());
    }
    if out.suggested && !out.fix {
        return Err("--suggested requires --fix".into());
    }
    Ok(out)
}

struct RenderedDiagnostic {
    file: String,
    start: u32,
    code: Code,
    message: String,
    severity: Severity,
    human: String,
    json: String,
}

fn render_diagnostic(diagnostic: &Diagnostic, sources: &SourceMap) -> RenderedDiagnostic {
    let file = diagnostic
        .source
        .and_then(|source| sources.get(source))
        .map(|file| file.name().to_string())
        .unwrap_or_default();
    let human = if diagnostic.code.is_lint() {
        diagnostic.render_lint(sources)
    } else {
        diagnostic.render(sources)
    };
    RenderedDiagnostic {
        file,
        start: diagnostic.span.map(|span| span.start).unwrap_or(0),
        code: diagnostic.code,
        message: diagnostic.message.clone(),
        severity: diagnostic.severity,
        human,
        json: diagnostic.render_json(sources),
    }
}

fn render_all(diagnostics: &[Diagnostic], sources: &SourceMap, out: &mut Vec<RenderedDiagnostic>) {
    for diagnostic in diagnostics {
        out.push(render_diagnostic(diagnostic, sources));
    }
}

fn print_rendered(diagnostics: &mut [RenderedDiagnostic], json: bool) {
    diagnostics.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then_with(|| left.start.cmp(&right.start))
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.message.cmp(&right.message))
    });
    let mut seen: HashSet<(String, u32, Code, String)> = HashSet::new();
    for diagnostic in diagnostics.iter() {
        let key = (
            diagnostic.file.clone(),
            diagnostic.start,
            diagnostic.code,
            diagnostic.message.clone(),
        );
        if !seen.insert(key) {
            continue;
        }
        if json {
            println!("{}", diagnostic.json);
        } else {
            eprintln!("{}", diagnostic.human);
        }
    }
}

fn lint_fails(diagnostics: &[RenderedDiagnostic], deny_warnings: bool) -> bool {
    diagnostics.iter().any(|diagnostic| {
        diagnostic.severity == Severity::Error
            || (deny_warnings && diagnostic.severity == Severity::Warning)
    })
}

fn find_nearest(start: &Path, name: &str) -> Option<PathBuf> {
    let start = if start.is_file() {
        start.parent()?
    } else {
        start
    };
    for dir in start.ancestors() {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn find_project_root(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        if dir.join("plugin.toml").is_file() || dir.join("martite.toml").is_file() {
            return Some(dir.to_path_buf());
        }
    }
    None
}

fn load_tool_config(start: &Path) -> Result<Option<(ToolConfig, PathBuf)>, String> {
    let Some(path) = find_nearest(start, "martite.toml") else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let config = parse_tool_config(&text).map_err(|error| error.message)?;
    let dir = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_path_buf();
    Ok(Some((config, dir)))
}

fn resolver_for(
    start: &Path,
    selected: Option<HashSet<Code>>,
) -> Result<(SettingsResolver, PathBuf), String> {
    let loaded = load_tool_config(start)?;
    let (config, dir) = match loaded {
        Some((config, dir)) => (config, dir),
        None => (
            ToolConfig::default(),
            if start.is_file() {
                start.parent().unwrap_or(Path::new(".")).to_path_buf()
            } else {
                start.to_path_buf()
            },
        ),
    };
    let resolver = SettingsResolver::new(&config, selected).map_err(|error| error.to_string())?;
    Ok((resolver, dir))
}

fn relative_to(base: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(base).unwrap_or(path);
    normalize_path(&relative.to_string_lossy())
}

fn same_path(left: &Path, right: &Path) -> bool {
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn scan_martite_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    scan_martite_files_into(root, &mut out);
    out.sort();
    out
}

fn scan_martite_files_into(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            scan_martite_files_into(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("mt") {
            out.push(path);
        }
    }
}

#[derive(Default)]
struct FmtArgs {
    check: bool,
    verify: bool,
    stdin: bool,
    paths: Vec<String>,
}

fn parse_fmt_args(args: &[String]) -> Result<FmtArgs, String> {
    use lexopt::prelude::*;

    let mut out = FmtArgs::default();
    let mut parser = lexopt::Parser::from_args(args.iter().cloned());
    while let Some(arg) = parser.next().map_err(|error| error.to_string())? {
        match arg {
            Long("check") => out.check = true,
            Long("verify") => out.verify = true,
            Long("stdin") => out.stdin = true,
            Value(value) => out
                .paths
                .push(value.string().map_err(|error| error.to_string())?),
            _ => return Err(arg.unexpected().to_string()),
        }
    }
    if out.stdin && !out.paths.is_empty() {
        return Err("--stdin cannot be combined with paths".into());
    }
    Ok(out)
}

fn cmd_fmt(args: &[String]) -> ExitCode {
    let parsed = match parse_fmt_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("error: {message}\n");
            return ExitCode::from(2);
        }
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    if parsed.stdin {
        let mut source = String::new();
        if let Err(error) = std::io::Read::read_to_string(&mut std::io::stdin(), &mut source) {
            eprintln!("error: cannot read stdin: {error}");
            return ExitCode::from(2);
        }
        let config = match fmt_config_for(&cwd) {
            Ok(config) => config,
            Err(message) => {
                eprintln!("error: {message}");
                return ExitCode::from(2);
            }
        };
        return format_stdin(&source, &config, &parsed, "<stdin>");
    }

    let files = match collect_fmt_files(&parsed, &cwd) {
        Ok(files) => files,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::from(2);
        }
    };

    let mut failed = false;
    for file in files {
        match format_file(&file, &parsed) {
            Ok(status) => {
                if status != ExitCode::SUCCESS {
                    failed = true;
                }
            }
            Err(message) => {
                eprintln!("error: {message}");
                return ExitCode::from(2);
            }
        }
    }
    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn fmt_config_for(start: &Path) -> Result<martite_config::FmtConfig, String> {
    match load_tool_config(start)? {
        Some((config, _)) => Ok(config.fmt),
        None => Ok(martite_config::FmtConfig::default()),
    }
}

fn collect_fmt_files(args: &FmtArgs, cwd: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    if args.paths.is_empty() {
        let root = if let Some(root) =
            discover_workspace_root(cwd).map_err(|error| error.message)?
        {
            root
        } else {
            let Some(root) = find_project_root(cwd) else {
                return Err(
                    "fmt requires a path when no `martite.toml` or `plugin.toml` is present".into(),
                );
            };
            root
        };
        scan_format_files(&root, &mut files);
    } else {
        for raw in &args.paths {
            let path = PathBuf::from(raw);
            if path.is_file() {
                let extension = path.extension().and_then(|ext| ext.to_str());
                if extension != Some("mt") && extension != Some("mti") {
                    return Err(format!("not a Martite source file: {}", path.display()));
                }
                files.push(path);
            } else if path.is_dir() {
                scan_format_files(&path, &mut files);
            } else {
                return Err(format!("path not found: {}", path.display()));
            }
        }
    }
    let mut seen = HashSet::new();
    files.retain(|path| {
        let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
        seen.insert(key)
    });
    files.sort();
    Ok(files)
}

fn scan_format_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            scan_format_files(&path, out);
        } else if matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("mt") | Some("mti")
        ) {
            out.push(path);
        }
    }
}

fn format_stdin(
    source: &str,
    config: &martite_config::FmtConfig,
    args: &FmtArgs,
    name: &str,
) -> ExitCode {
    let output = martite_fmt::format_source(source, config);
    if output.text.is_none() {
        print_fmt_diagnostics(&output.diagnostics, name, source);
        return ExitCode::from(1);
    }
    let formatted = output.text.unwrap();
    if args.verify
        && let Err(message) = verify_format(source, &formatted, false)
    {
        eprintln!("error: formatter bug in {name}: {message}");
        return ExitCode::from(2);
    }
    if args.check {
        if source != formatted {
            print!("{}", unified_diff(name, source, &formatted));
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    print!("{formatted}");
    ExitCode::SUCCESS
}

fn format_file(path: &Path, args: &FmtArgs) -> Result<ExitCode, String> {
    let source = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let interface = path.extension().and_then(|ext| ext.to_str()) == Some("mti");
    let config = fmt_config_for(path)?;
    let output = if interface {
        martite_fmt::format_interface(&source, &config)
    } else {
        martite_fmt::format_source(&source, &config)
    };
    let name = path
        .strip_prefix(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .unwrap_or(path)
        .display()
        .to_string();
    if output.text.is_none() {
        print_fmt_diagnostics(&output.diagnostics, &name, &source);
        return Ok(ExitCode::from(1));
    }
    let formatted = output.text.unwrap();
    if args.verify {
        verify_format(&source, &formatted, interface)
            .map_err(|message| format!("formatter bug in {name}: {message}"))?;
    }
    if args.check {
        if source != formatted {
            print!("{}", unified_diff(&name, &source, &formatted));
            return Ok(ExitCode::from(1));
        }
        return Ok(ExitCode::SUCCESS);
    }
    if source != formatted {
        std::fs::write(path, formatted)
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    }
    Ok(ExitCode::SUCCESS)
}

fn verify_format(before: &str, after: &str, interface: bool) -> Result<(), String> {
    let left = if interface {
        parse_interface(before, LoadLimits::default())
    } else {
        parse_source(before, LoadLimits::default())
    };
    let right = if interface {
        parse_interface(after, LoadLimits::default())
    } else {
        parse_source(after, LoadLimits::default())
    };
    if right.diagnostics.iter().any(|diagnostic| {
        matches!(diagnostic.code, Code::E001 | Code::E002 | Code::E003)
            && diagnostic.severity == Severity::Error
    }) {
        return Err("formatted output does not parse".into());
    }
    if !martite_fmt::structurally_equal(before, &left.program, after, &right.program) {
        return Err("formatted output has a different AST".into());
    }
    Ok(())
}

fn print_fmt_diagnostics(diagnostics: &[martite_base::Diagnostic], name: &str, source: &str) {
    let mut sources = SourceMap::new();
    let source_id = sources.add(name.to_string(), source.to_string());
    for diagnostic in diagnostics {
        if !matches!(diagnostic.code, Code::E001 | Code::E002 | Code::E003) {
            continue;
        }
        let mut diagnostic = diagnostic.clone();
        diagnostic.source = Some(source_id);
        eprintln!("{}", diagnostic.render(&sources));
    }
}

/// A compact whole-file unified diff. Formatter output is already canonical,
/// so a single hunk is both valid and easier to read than a minimal edit
/// script with many tiny hunks.
fn unified_diff(name: &str, before: &str, after: &str) -> String {
    let before_lines: Vec<&str> = before
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    let after_lines: Vec<&str> = after
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    let before_count = if before.ends_with('\n') || before.ends_with("\r\n") {
        before_lines.len() - 1
    } else {
        before_lines.len()
    };
    let after_count = if after.ends_with('\n') || after.ends_with("\r\n") {
        after_lines.len() - 1
    } else {
        after_lines.len()
    };
    let name = name.replace('\\', "/");
    let mut out = String::new();
    out.push_str(&format!("--- a/{name}\n"));
    out.push_str(&format!("+++ b/{name}\n"));
    out.push_str(&format!("@@ -1,{before_count} +1,{after_count} @@\n"));
    for line in before_lines.iter().take(before_count) {
        out.push('-');
        out.push_str(line);
        out.push('\n');
    }
    for line in after_lines.iter().take(after_count) {
        out.push('+');
        out.push_str(line);
        out.push('\n');
    }
    out
}

struct ProjectSpec {
    root: PathBuf,
    entry: String,
    edition: String,
    /// Host interfaces available to the load, from the workspace and
    /// repeatable `--interface` flags.
    interfaces: Vec<InterfaceSource>,
    /// Modules granted by a plugin manifest.
    grants: Vec<String>,
    manifest: Option<Manifest>,
    manifest_text: Option<String>,
    /// Prefix used when rendering source names from a workspace member.
    display_prefix: String,
}

impl ProjectSpec {
    fn is_script(&self) -> bool {
        self.manifest.is_none()
    }

    fn display_sources(&self, sources: &SourceMap) -> SourceMap {
        sources.prefixed(&self.display_prefix)
    }

    /// Interfaces a static load sees: the caller's `.mti` files plus the
    /// standalone std modules, which only scripts may use.
    fn static_interfaces(&self) -> Vec<InterfaceSource> {
        let mut interfaces = self.interfaces.clone();
        if self.is_script() {
            for module in StandaloneHost::interfaces() {
                if !interfaces
                    .iter()
                    .any(|interface| interface.path == module.path)
                {
                    interfaces.push(module);
                }
            }
        }
        interfaces
    }

    /// Grants a static load sees. Scripts grant every registered interface;
    /// plugins grant only what their manifest lists.
    fn static_grants(&self) -> Vec<String> {
        if self.is_script() {
            interface_paths(&self.static_interfaces())
        } else {
            self.grants.clone()
        }
    }

    fn load_static(&self) -> LoadedProgram {
        let provider = FileSystemProvider::new(self.root.clone());
        load(
            &provider,
            &LoadOptions {
                entry: self.entry.clone(),
                interfaces: self.static_interfaces(),
                grants: self.static_grants(),
                limits: LoadLimits::default(),
                edition: self.edition.clone(),
            },
        )
    }
}

/// The standalone host's pure modules. `martite run` and `martite test`
/// register them for scripts; plugins never see them.
struct StandaloneHost {
    script_args: Vec<String>,
}

impl StandaloneHost {
    const MODULES: [(&'static str, &'static str); 2] = [("std.io", STD_IO), ("std.env", STD_ENV)];

    fn interfaces() -> Vec<InterfaceSource> {
        Self::MODULES
            .iter()
            .map(|(path, text)| InterfaceSource {
                path: (*path).into(),
                text: (*text).into(),
            })
            .collect()
    }
}

fn interface_paths(interfaces: &[InterfaceSource]) -> Vec<String> {
    interfaces
        .iter()
        .map(|interface| interface.path.clone())
        .collect()
}

fn script_spec(
    root: PathBuf,
    entry: String,
    cli: &[PathBuf],
    workspace: Option<&Workspace>,
) -> Result<ProjectSpec, String> {
    let interfaces = merge_interfaces(
        workspace
            .map(|workspace| workspace.interfaces.clone())
            .unwrap_or_default(),
        load_interfaces(cli, Some(&root))?,
    )?;
    let display_prefix = workspace
        .map(|workspace| relative_to(&workspace.root, &root))
        .unwrap_or_default();
    Ok(ProjectSpec {
        root,
        entry,
        edition: LANG_EDITION.to_string(),
        interfaces,
        grants: Vec::new(),
        manifest: None,
        manifest_text: None,
        display_prefix,
    })
}

/// Why a single-project path could not be resolved.
enum ProjectError {
    /// Bad invocation, such as a missing path or manifest. Exit 2.
    Usage(String),
    /// A diagnostic the command reports, such as E508.
    Diagnostic(Diagnostic),
}

fn resolve_project(path: &Path, interfaces: &[PathBuf]) -> Result<ProjectSpec, ProjectError> {
    let workspace = discover_workspace(path).map_err(|error| ProjectError::Usage(error.message))?;
    if path.is_dir() {
        return load_plugin_spec(path, interfaces, workspace.as_ref());
    }
    if !path.is_file() {
        return Err(ProjectError::Usage(format!(
            "path not found: {}",
            path.display()
        )));
    }
    let root = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let entry = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ProjectError::Usage(format!("invalid path `{}`", path.display())))?
        .to_string();
    script_spec(root, entry, interfaces, workspace.as_ref()).map_err(ProjectError::Usage)
}

fn load_plugin_spec(
    root: &Path,
    cli: &[PathBuf],
    workspace: Option<&Workspace>,
) -> Result<ProjectSpec, ProjectError> {
    let manifest_path = root.join("plugin.toml");
    let text = std::fs::read_to_string(&manifest_path).map_err(|error| {
        ProjectError::Usage(format!("cannot read {}: {error}", manifest_path.display()))
    })?;
    let manifest = parse_manifest(&text).map_err(|error| {
        ProjectError::Diagnostic(Diagnostic::error(
            Code::E508,
            format!("{}: {}", manifest_path.display(), error.message),
        ))
    })?;
    let interfaces = merge_interfaces(
        workspace
            .map(|workspace| workspace.interfaces.clone())
            .unwrap_or_default(),
        load_interfaces(cli, Some(root)).map_err(ProjectError::Usage)?,
    )
    .map_err(ProjectError::Usage)?;
    let display_prefix = workspace
        .map(|workspace| relative_to(&workspace.root, root))
        .unwrap_or_default();
    Ok(ProjectSpec {
        root: root.to_path_buf(),
        entry: manifest.entry.clone(),
        edition: manifest.lang.clone(),
        grants: manifest.grants.clone(),
        interfaces,
        manifest: Some(manifest),
        manifest_text: Some(text),
        display_prefix,
    })
}

fn exec_limits(args: &CliArgs) -> ExecLimits {
    let defaults = ExecLimits::default();
    ExecLimits {
        fuel: args.fuel.unwrap_or(defaults.fuel),
        memory: args.memory.unwrap_or(defaults.memory),
        depth: args.depth.unwrap_or(defaults.depth),
        deadline: None,
    }
}

/// Build the execution host for one project.
///
/// `run` and `test` share this: the same interfaces, standalone std modules
/// (scripts only), optimizer switch and limits. `check` and `lint` stay on the
/// static driver loader so they never require Rust host implementations.
fn build_engine(spec: &ProjectSpec, args: &CliArgs, script_args: Vec<String>) -> Engine {
    let mut engine = Engine::new();
    engine.set_optimize(!args.no_optimize);
    engine.set_verification(CLI_VERIFICATION);
    let limits = exec_limits(args);
    engine.set_limits(limits, limits);
    for interface in &spec.interfaces {
        engine.interface(interface.path.clone(), interface.text.clone());
    }
    if spec.is_script() {
        StandaloneHost { script_args }.register(&mut engine, &spec.interfaces);
    }
    engine
}

fn load_executable(
    spec: &ProjectSpec,
    engine: &Engine,
    provider: &dyn SourceProvider,
) -> Result<Plugin, LoadError> {
    match &spec.manifest {
        Some(manifest) => engine.try_load_with_manifest(manifest.clone(), provider),
        None => engine.try_load_script(spec.entry.clone(), provider),
    }
}

fn collect_projects(
    args: &LintArgs,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Vec<ProjectSpec>, String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut specs = Vec::new();

    if args.paths.is_empty() {
        if let Some(workspace) = discover_workspace(&cwd).map_err(|error| error.message)? {
            if workspace.members.is_empty() {
                return Err("workspace has no members".into());
            }
            specs.extend(workspace_default_specs(
                &workspace,
                &cwd,
                &args.interfaces,
                diagnostics,
            ));
        } else {
            let Some(root) = find_project_root(&cwd) else {
                return Err(
                    "lint requires a path when no `martite.toml` or `plugin.toml` is present"
                        .into(),
                );
            };
            let workspace = discover_workspace(&root).map_err(|error| error.message)?;
            if root.join("plugin.toml").is_file() {
                if let Some(spec) =
                    plugin_spec(&root, &args.interfaces, workspace.as_ref(), diagnostics)
                {
                    specs.push(spec);
                }
            } else {
                for file in scan_martite_files(&root) {
                    let entry = relative_to(&root, &file);
                    specs.push(script_spec(
                        root.clone(),
                        entry,
                        &args.interfaces,
                        workspace.as_ref(),
                    )?);
                }
            }
        }
    } else {
        for raw in &args.paths {
            let path = PathBuf::from(raw);
            if path.is_file() {
                if path.extension().and_then(|ext| ext.to_str()) == Some("mti") {
                    continue;
                }
                let root = path
                    .parent()
                    .filter(|path| !path.as_os_str().is_empty())
                    .unwrap_or(Path::new("."))
                    .to_path_buf();
                let entry = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| format!("invalid path `{raw}`"))?
                    .to_string();
                let workspace = discover_workspace(&path).map_err(|error| error.message)?;
                specs.push(script_spec(
                    root,
                    entry,
                    &args.interfaces,
                    workspace.as_ref(),
                )?);
            } else if path.is_dir() {
                let workspace = discover_workspace(&path).map_err(|error| error.message)?;
                if let Some(workspace) = workspace.as_ref()
                    && same_path(&path, &workspace.root)
                {
                    if workspace.members.is_empty() {
                        return Err("workspace has no members".into());
                    }
                    specs.extend(workspace_specs(workspace, &args.interfaces, diagnostics));
                    continue;
                }
                if path.join("plugin.toml").is_file() {
                    if let Some(spec) =
                        plugin_spec(&path, &args.interfaces, workspace.as_ref(), diagnostics)
                    {
                        specs.push(spec);
                    }
                } else {
                    for file in scan_martite_files(&path) {
                        let entry = relative_to(&path, &file);
                        specs.push(script_spec(
                            path.clone(),
                            entry,
                            &args.interfaces,
                            workspace.as_ref(),
                        )?);
                    }
                }
            } else {
                return Err(format!("path not found: {}", path.display()));
            }
        }
    }

    // Files reached through more than one entry are linted once.
    let mut seen = HashSet::new();
    specs.retain(|spec| {
        let full = spec.root.join(&spec.entry);
        let key = std::fs::canonicalize(&full).unwrap_or(full);
        seen.insert(key)
    });
    Ok(specs)
}

fn plugin_spec(
    root: &Path,
    cli: &[PathBuf],
    workspace: Option<&Workspace>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ProjectSpec> {
    match load_plugin_spec(root, cli, workspace) {
        Ok(spec) => Some(spec),
        Err(ProjectError::Diagnostic(diagnostic)) => {
            diagnostics.push(diagnostic);
            None
        }
        Err(ProjectError::Usage(message)) => {
            diagnostics.push(Diagnostic::error(Code::E508, message));
            None
        }
    }
}

fn workspace_specs(
    workspace: &Workspace,
    cli: &[PathBuf],
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<ProjectSpec> {
    let mut specs = Vec::new();
    let mut names: HashMap<String, PathBuf> = HashMap::new();
    for member in &workspace.members {
        let Some(spec) = plugin_spec(member, cli, Some(workspace), diagnostics) else {
            continue;
        };
        if let Some(manifest) = &spec.manifest {
            if let Some(previous) = names.get(&manifest.name) {
                diagnostics.push(Diagnostic::error(
                    Code::E508,
                    format!(
                        "duplicate plugin name `{}` in {} and {}",
                        manifest.name,
                        previous.display(),
                        member.display()
                    ),
                ));
                continue;
            }
            names.insert(manifest.name.clone(), member.clone());
        }
        specs.push(spec);
    }
    specs
}

/// Specs for a no-path command. At the workspace root this is every member;
/// from inside a member it is that member, like Cargo.
fn workspace_default_specs(
    workspace: &Workspace,
    cwd: &Path,
    cli: &[PathBuf],
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<ProjectSpec> {
    if !same_path(cwd, &workspace.root)
        && let Some(root) = find_project_root(cwd)
        && !same_path(&root, &workspace.root)
    {
        return plugin_spec(&root, cli, Some(workspace), diagnostics)
            .into_iter()
            .collect();
    }
    workspace_specs(workspace, cli, diagnostics)
}

struct OverlayProvider<'a> {
    base: &'a dyn SourceProvider,
    overlay: &'a HashMap<String, String>,
}

impl SourceProvider for OverlayProvider<'_> {
    fn read(&self, path: &str) -> Result<String, String> {
        if let Some(text) = self.overlay.get(path) {
            Ok(text.clone())
        } else {
            self.base.read(path)
        }
    }
}

#[derive(Clone)]
struct PendingEdit {
    path: String,
    span: martite_base::Span,
    replacement: String,
}

struct PendingFix {
    code: Code,
    edits: Vec<PendingEdit>,
}

struct FixOutcome {
    rendered: Vec<RenderedDiagnostic>,
    failed: bool,
}

fn fix_project(
    spec: &ProjectSpec,
    resolver: &SettingsResolver,
    config_dir: &Path,
    suggested: bool,
) -> FixOutcome {
    let base = FileSystemProvider::new(spec.root.clone());
    let mut overlay: HashMap<String, String> = HashMap::new();
    let mut last = Vec::new();

    for pass in 0..=5 {
        let manifest_text = spec.manifest_text.as_ref().map(|original| {
            overlay
                .get("plugin.toml")
                .cloned()
                .unwrap_or_else(|| original.clone())
        });
        let (entry, edition, grants, manifest) = if let Some(text) = &manifest_text {
            match parse_manifest(text) {
                Ok(manifest) => (
                    manifest.entry.clone(),
                    manifest.lang.clone(),
                    manifest.grants.clone(),
                    Some(manifest),
                ),
                Err(error) => {
                    let diagnostic = Diagnostic::error(Code::E508, error.message);
                    last = vec![render_diagnostic(&diagnostic, &SourceMap::new())];
                    return FixOutcome {
                        rendered: last,
                        failed: true,
                    };
                }
            }
        } else {
            (
                spec.entry.clone(),
                spec.edition.clone(),
                spec.static_grants(),
                None,
            )
        };

        let provider = OverlayProvider {
            base: &base,
            overlay: &overlay,
        };
        let mut loaded = load(
            &provider,
            &LoadOptions {
                entry,
                interfaces: spec.static_interfaces(),
                grants,
                limits: LoadLimits::default(),
                edition,
            },
        );
        last.clear();
        let display_sources = spec.display_sources(&loaded.sources);
        render_all(&loaded.diagnostics, &display_sources, &mut last);
        if loaded
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
        {
            return FixOutcome {
                rendered: last,
                failed: true,
            };
        }
        let Some(program) = loaded.program.as_ref() else {
            return FixOutcome {
                rendered: last,
                failed: false,
            };
        };
        let manifest_source = manifest_text
            .as_ref()
            .map(|text| loaded.sources.add("plugin.toml", text.clone()));
        let inputs = match (&manifest, manifest_source, manifest_text.as_deref()) {
            (Some(manifest), Some(source), Some(text)) => LintInputs {
                manifest: Some(ManifestInput {
                    source,
                    text,
                    grants: &manifest.grants,
                }),
            },
            _ => LintInputs::default(),
        };
        let prefix = relative_to(config_dir, &spec.root);
        let output = martite_lint::lint(program, &loaded.sources, resolver, &prefix, &inputs);
        let display_sources = spec.display_sources(&loaded.sources);
        render_all(&output.diagnostics, &display_sources, &mut last);

        if pass == 5 {
            break;
        }
        let edits = collect_edits(&output.diagnostics, &loaded.sources, suggested);
        if edits.is_empty() {
            break;
        }
        if !apply_edits(&edits, &loaded.sources, &mut overlay) {
            break;
        }
    }

    let mut failed = false;
    for (path, text) in &overlay {
        let full = spec.root.join(path);
        if let Some(parent) = full.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            eprintln!("error: cannot create {}: {error}", parent.display());
            failed = true;
            continue;
        }
        if let Err(error) = std::fs::write(&full, text) {
            eprintln!("error: cannot write {}: {error}", full.display());
            failed = true;
        }
    }
    FixOutcome {
        rendered: last,
        failed,
    }
}

fn collect_edits(
    diagnostics: &[Diagnostic],
    sources: &SourceMap,
    suggested: bool,
) -> Vec<PendingEdit> {
    let mut fixes: Vec<PendingFix> = Vec::new();
    for diagnostic in diagnostics {
        let Some(fix) = &diagnostic.fix else {
            continue;
        };
        if fix.applicability == Applicability::Maybe && !suggested {
            continue;
        }
        let mut fix_edits = Vec::new();
        let mut complete = true;
        for edit in &fix.edits {
            let source = edit.source.or(diagnostic.source);
            let Some(source) = source else {
                complete = false;
                break;
            };
            let Some(file) = sources.get(source) else {
                complete = false;
                break;
            };
            if edit.span.start > edit.span.end || edit.span.end as usize > file.text().len() {
                complete = false;
                break;
            }
            fix_edits.push(PendingEdit {
                path: file.name().to_string(),
                span: edit.span,
                replacement: edit.replacement.clone(),
            });
        }
        if complete && !fix_edits.is_empty() {
            fixes.push(PendingFix {
                code: diagnostic.code,
                edits: fix_edits,
            });
        }
    }
    fixes.sort_by_key(|fix| {
        (
            fix.edits
                .iter()
                .map(|edit| edit.span.start)
                .min()
                .unwrap_or(u32::MAX),
            fix.code,
        )
    });
    let mut accepted: Vec<PendingEdit> = Vec::new();
    for fix in fixes {
        // A fix is atomic: all of its edits are accepted, or none are.
        if fix
            .edits
            .iter()
            .any(|edit| accepted.iter().any(|previous| interferes(previous, edit)))
        {
            continue;
        }
        accepted.extend(fix.edits);
    }
    accepted
}

fn interferes(left: &PendingEdit, right: &PendingEdit) -> bool {
    if left.path != right.path {
        return false;
    }
    let left_empty = left.span.start == left.span.end;
    let right_empty = right.span.start == right.span.end;
    if left_empty && right_empty {
        return left.span.start == right.span.start;
    }
    if left_empty {
        return left.span.start >= right.span.start && left.span.start <= right.span.end;
    }
    if right_empty {
        return right.span.start >= left.span.start && right.span.start <= left.span.end;
    }
    left.span.start < right.span.end && right.span.start < left.span.end
}

fn apply_edits(
    edits: &[PendingEdit],
    sources: &SourceMap,
    overlay: &mut HashMap<String, String>,
) -> bool {
    let mut by_path: HashMap<String, Vec<&PendingEdit>> = HashMap::new();
    for edit in edits {
        by_path.entry(edit.path.clone()).or_default().push(edit);
    }
    let mut changed = false;
    for (path, mut edits) in by_path {
        let Some(file) = sources.files().find(|(_, file)| file.name() == path) else {
            continue;
        };
        let mut text = file.1.text().to_string();
        edits.sort_by(|left, right| {
            right
                .span
                .start
                .cmp(&left.span.start)
                .then_with(|| right.span.end.cmp(&left.span.end))
        });
        let mut applied = false;
        for edit in edits {
            let start = edit.span.start as usize;
            let end = edit.span.end as usize;
            if start > text.len() || end > text.len() || start > end {
                continue;
            }
            text.replace_range(start..end, &edit.replacement);
            applied = true;
        }
        if applied {
            changed = true;
            overlay.insert(path, text);
        }
    }
    changed
}

fn cmd_lint(args: &[String]) -> ExitCode {
    let parsed = match parse_lint_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("error: {message}\n");
            return ExitCode::from(2);
        }
    };

    if parsed.list_rules {
        if parsed.json {
            println!("{}", martite_lint::list_rules_json());
        } else {
            print!("{}", martite_lint::list_rules_text());
        }
        return ExitCode::SUCCESS;
    }
    if let Some(id) = &parsed.explain {
        let Some(code) = Code::from_str(id) else {
            eprintln!("error: unknown rule `{id}`");
            return ExitCode::from(2);
        };
        if !code.is_lint() || martite_lint::is_retired(code) || !martite_lint::is_known(code) {
            eprintln!("error: unknown lint rule `{id}`");
            return ExitCode::from(2);
        }
        match martite_lint::explain_text(code) {
            Some(text) => {
                println!("{text}");
                return ExitCode::SUCCESS;
            }
            None => {
                eprintln!("error: unknown lint rule `{id}`");
                return ExitCode::from(2);
            }
        }
    }

    if parsed.syntax_only {
        return lint_syntax_only_mode(&parsed);
    }

    let mut project_diagnostics = Vec::new();
    let specs = match collect_projects(&parsed, &mut project_diagnostics) {
        Ok(specs) => specs,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::from(2);
        }
    };

    let mut rendered = Vec::new();
    let empty_sources = SourceMap::new();
    render_all(&project_diagnostics, &empty_sources, &mut rendered);
    let mut had_load_error = !project_diagnostics.is_empty();

    for spec in specs {
        let (resolver, config_dir) = match resolver_for(&spec.root, parsed.rules.clone()) {
            Ok(value) => value,
            Err(message) => {
                eprintln!("error: {message}");
                return ExitCode::from(2);
            }
        };
        if parsed.fix {
            let outcome = fix_project(&spec, &resolver, &config_dir, parsed.suggested);
            rendered.extend(outcome.rendered);
            if outcome.failed {
                had_load_error = true;
            }
            continue;
        }
        let mut loaded = spec.load_static();
        let display_sources = spec.display_sources(&loaded.sources);
        render_all(&loaded.diagnostics, &display_sources, &mut rendered);
        if loaded
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
        {
            had_load_error = true;
            continue;
        }
        let Some(program) = loaded.program.as_ref() else {
            continue;
        };
        let manifest_source = spec
            .manifest_text
            .as_ref()
            .map(|text| loaded.sources.add("plugin.toml", text.clone()));
        let inputs = match (&spec.manifest, manifest_source, &spec.manifest_text) {
            (Some(manifest), Some(source), Some(text)) => LintInputs {
                manifest: Some(ManifestInput {
                    source,
                    text,
                    grants: &manifest.grants,
                }),
            },
            _ => LintInputs::default(),
        };
        let prefix = relative_to(&config_dir, &spec.root);
        let output = martite_lint::lint(program, &loaded.sources, &resolver, &prefix, &inputs);
        let display_sources = spec.display_sources(&loaded.sources);
        render_all(&output.diagnostics, &display_sources, &mut rendered);
    }
    print_rendered(&mut rendered, parsed.json);
    if had_load_error || lint_fails(&rendered, parsed.deny_warnings) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn lint_syntax_only_mode(args: &LintArgs) -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut files = Vec::new();
    if args.paths.is_empty() {
        let root = match discover_workspace_root(&cwd) {
            Ok(Some(root)) => root,
            Ok(None) => {
                let Some(root) = find_project_root(&cwd) else {
                    eprintln!(
                        "error: --syntax-only requires a path when no project file is present"
                    );
                    return ExitCode::from(2);
                };
                root
            }
            Err(error) => {
                eprintln!("error: {}", error.message);
                return ExitCode::from(2);
            }
        };
        files.extend(scan_martite_files(&root));
    } else {
        for raw in &args.paths {
            let path = PathBuf::from(raw);
            if path.is_file() {
                if path.extension().and_then(|ext| ext.to_str()) != Some("mti") {
                    files.push(path);
                }
            } else if path.is_dir() {
                files.extend(scan_martite_files(&path));
            } else {
                eprintln!("error: path not found: {}", path.display());
                return ExitCode::from(2);
            }
        }
    }
    let mut seen = HashSet::new();
    files.retain(|path| {
        let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
        seen.insert(key)
    });

    let mut sources = SourceMap::new();
    let mut diagnostics = Vec::new();
    let mut had_parse_error = false;
    for path in files {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => normalize_source(&text).into_owned(),
            Err(error) => {
                eprintln!("error: cannot read {}: {error}", path.display());
                return ExitCode::from(2);
            }
        };
        let display = path
            .strip_prefix(&cwd)
            .unwrap_or(&path)
            .display()
            .to_string();
        let source = sources.add(display, text.clone());
        let parsed = parse_source(&text, LoadLimits::default());
        if !parsed.diagnostics.is_empty() {
            had_parse_error = true;
            let mut parse_diagnostics = parsed.diagnostics;
            for diagnostic in &mut parse_diagnostics {
                diagnostic.source = Some(source);
            }
            diagnostics.extend(parse_diagnostics);
            continue;
        }
        let (resolver, config_dir) = match resolver_for(&path, args.rules.clone()) {
            Ok(value) => value,
            Err(message) => {
                eprintln!("error: {message}");
                return ExitCode::from(2);
            }
        };
        let relative = relative_to(&config_dir, &path);
        let settings = resolver.resolve(&relative);
        diagnostics.extend(martite_lint::lint_syntax_only(
            source,
            &text,
            &parsed.program,
            &settings,
        ));
    }
    sort_diagnostics(&mut diagnostics, &sources);
    let mut rendered = Vec::new();
    render_all(&diagnostics, &sources, &mut rendered);
    print_rendered(&mut rendered, args.json);
    if had_parse_error || lint_fails(&rendered, args.deny_warnings) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn cmd_run(raw: &[String]) -> ExitCode {
    let args = match parse_args(raw, Command::Run) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("error: {message}\n");
            return ExitCode::from(2);
        }
    };
    let Some(file) = args.path.clone() else {
        eprintln!("error: run requires a file\n");
        return ExitCode::from(2);
    };
    let path = PathBuf::from(&file);
    if !path.is_file() {
        eprintln!("error: file not found: {file}");
        return ExitCode::from(2);
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        eprintln!("error: invalid file path `{file}`");
        return ExitCode::from(2);
    };
    let root = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let spec = match script_spec(root.clone(), name.to_string(), &args.interfaces, None) {
        Ok(spec) => spec,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::from(2);
        }
    };
    let engine = build_engine(&spec, &args, args.script_args.clone());
    // `martite run` is the standalone host: it is intentionally unrestricted.
    let provider = FileSystemProvider::unrestricted(root);
    let plugin = match load_executable(&spec, &engine, &provider) {
        Ok(plugin) => plugin,
        Err(error) => {
            print_diagnostics(&error.diagnostics, &error.sources, args.json);
            return ExitCode::from(2);
        }
    };
    if let Some(stage) = args.dump_ir {
        dump_ir(stage, plugin.lowered_ir().as_ref(), plugin.ir());
    }
    match plugin.run_main() {
        Ok(main) => {
            for line in main.log {
                println!("{line}");
            }
            match main.value {
                Value::Result(Err(error)) => {
                    eprintln!("{}", martite_runtime::show(&error, plugin.types(), true));
                    ExitCode::from(1)
                }
                _ => ExitCode::SUCCESS,
            }
        }
        Err(trap) => {
            print_trap(&trap, plugin.sources());
            ExitCode::from(101)
        }
    }
}

fn command_specs(
    command: &str,
    path: Option<&str>,
    cli: &[PathBuf],
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Vec<ProjectSpec>, String> {
    if let Some(path) = path {
        let path = PathBuf::from(path);
        return match resolve_project(&path, cli) {
            Ok(spec) => Ok(vec![spec]),
            Err(ProjectError::Usage(message)) => Err(message),
            Err(ProjectError::Diagnostic(diagnostic)) => {
                diagnostics.push(diagnostic);
                Ok(Vec::new())
            }
        };
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let Some(workspace) = discover_workspace(&cwd).map_err(|error| error.message)? else {
        return Err(format!("{command} requires a path"));
    };
    if workspace.members.is_empty() {
        return Err("workspace has no members".into());
    }
    Ok(workspace_default_specs(&workspace, &cwd, cli, diagnostics))
}

fn cmd_check(raw: &[String]) -> ExitCode {
    let args = match parse_args(raw, Command::Check) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("error: {message}\n");
            return ExitCode::from(2);
        }
    };
    let mut project_diagnostics = Vec::new();
    let specs = match command_specs(
        "check",
        args.path.as_deref(),
        &args.interfaces,
        &mut project_diagnostics,
    ) {
        Ok(specs) => specs,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::from(2);
        }
    };

    print_diagnostics(&project_diagnostics, &SourceMap::new(), args.json);
    let mut failed = !project_diagnostics.is_empty();
    for spec in specs {
        let loaded = spec.load_static();
        let sources = spec.display_sources(&loaded.sources);
        print_diagnostics(&loaded.diagnostics, &sources, args.json);
        failed |= loaded.has_errors();
    }
    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn cmd_lower(raw: &[String]) -> ExitCode {
    let args = match parse_args(raw, Command::Lower) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("error: {message}\n");
            return ExitCode::from(2);
        }
    };
    let Some(file) = args.path.clone() else {
        eprintln!("error: lower requires a file\n");
        return ExitCode::from(2);
    };
    let path = PathBuf::from(&file);
    if !path.is_file() {
        eprintln!("error: file not found: {file}");
        return ExitCode::from(2);
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        eprintln!("error: invalid file path `{file}`");
        return ExitCode::from(2);
    };
    let root = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let spec = match script_spec(root, name.to_string(), &args.interfaces, None) {
        Ok(spec) => spec,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::from(2);
        }
    };
    let loaded = spec.load_static();
    print_diagnostics(&loaded.diagnostics, &loaded.sources, args.json);
    if loaded.has_errors() {
        return ExitCode::from(1);
    }
    let Some(program) = loaded.program.as_ref() else {
        eprintln!("error: no program");
        return ExitCode::from(2);
    };
    let Some(mut ir) = lower_for_dump(program, &loaded.sources, args.json) else {
        return ExitCode::from(1);
    };
    if args.optimize && !optimize_ir(&mut ir, args.json) {
        return ExitCode::from(1);
    }
    print!("{}", Printer::new(&ir).program());
    ExitCode::SUCCESS
}

fn optimize_ir(ir: &mut IrProgram, json: bool) -> bool {
    match martite_opt::optimize(
        ir,
        martite_opt::OptimizeOptions {
            verify: CLI_VERIFICATION,
            ..Default::default()
        },
    ) {
        Ok(_) => true,
        Err(error) => {
            let diagnostic = Diagnostic::error(Code::E508, format!("optimization failed: {error}"));
            print_diagnostics(&[diagnostic], &SourceMap::new(), json);
            false
        }
    }
}

fn cmd_test(raw: &[String]) -> ExitCode {
    let args = match parse_args(raw, Command::Test) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("error: {message}\n");
            return ExitCode::from(2);
        }
    };
    let mut project_diagnostics = Vec::new();
    let specs = match command_specs(
        "test",
        args.path.as_deref(),
        &args.interfaces,
        &mut project_diagnostics,
    ) {
        Ok(specs) => specs,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::from(2);
        }
    };

    print_diagnostics(&project_diagnostics, &SourceMap::new(), args.json);
    let mut failed_run = !project_diagnostics.is_empty();
    let multiple = specs.len() > 1;
    let mut total_all = 0usize;
    let mut failed_all = 0usize;

    for spec in specs {
        let label = spec
            .manifest
            .as_ref()
            .map(|manifest| manifest.name.clone())
            .unwrap_or_else(|| spec.entry.clone());
        let mut engine = build_engine(&spec, &args, Vec::new());
        // Tests run without a double for every host function: an unregistered
        // call traps with `no test double` instead of failing the load.
        engine.set_allow_missing_host_impls(true);
        let provider = FileSystemProvider::new(spec.root.clone());
        let plugin = match load_executable(&spec, &engine, &provider) {
            Ok(plugin) => plugin,
            Err(error) => {
                let sources = spec.display_sources(&error.sources);
                print_diagnostics(&error.diagnostics, &sources, args.json);
                failed_run = true;
                continue;
            }
        };
        if let Some(stage) = args.dump_ir {
            dump_ir(stage, plugin.lowered_ir().as_ref(), plugin.ir());
        }
        let mut failed = 0usize;
        let mut total = 0usize;
        for report in plugin.run_tests() {
            total += 1;
            for line in report.log {
                println!("{line}");
            }
            match report.outcome {
                Ok(_) => {
                    if multiple {
                        println!("{label}: test {} ... ok", report.name);
                    } else {
                        println!("test {} ... ok", report.name);
                    }
                }
                Err(trap) => {
                    failed += 1;
                    if multiple {
                        println!(
                            "{label}: test {} ... FAILED: {}",
                            report.name,
                            trap.panic_message()
                        );
                    } else {
                        println!("test {} ... FAILED: {}", report.name, trap.panic_message());
                    }
                }
            }
        }
        if multiple {
            if total == 0 {
                println!("{label}: no tests found");
            } else {
                println!("{label}: {}/{} tests passed", total - failed, total);
            }
        }
        total_all += total;
        failed_all += failed;
        if failed > 0 {
            failed_run = true;
        }
    }

    if multiple {
        if total_all == 0 {
            println!("no tests found");
        } else {
            println!("{}/{} tests passed", total_all - failed_all, total_all);
        }
    } else if total_all == 0 {
        println!("no tests found");
    } else {
        println!("{}/{} tests passed", total_all - failed_all, total_all);
    }

    if failed_run {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn print_diagnostics(diagnostics: &[Diagnostic], sources: &SourceMap, json: bool) {
    for diagnostic in diagnostics {
        if json {
            println!("{}", diagnostic.render_json(sources));
        } else {
            eprintln!("{}", diagnostic.render(sources));
        }
    }
}

fn dump_ir(stage: DumpStage, lowered: Option<&IrProgram>, executing: &IrProgram) {
    if matches!(stage, DumpStage::Before | DumpStage::Both) {
        let before = lowered.unwrap_or(executing);
        eprintln!("; IR before optimization");
        eprint!("{}", Printer::new(before).program());
    }
    if matches!(stage, DumpStage::After | DumpStage::Both) {
        eprintln!("; IR after optimization");
        eprint!("{}", Printer::new(executing).program());
    }
}

fn lower_for_dump(program: &CheckedProgram, sources: &SourceMap, json: bool) -> Option<IrProgram> {
    let lowered = lower_with(
        program,
        sources,
        LowerOptions {
            verify: CLI_VERIFICATION,
        },
    );
    if lowered.has_errors() {
        print_diagnostics(&lowered.diagnostics, sources, json);
        return None;
    }
    match lowered.program {
        Some(program) => Some(program),
        None => {
            eprintln!("error: lowering produced no IR program");
            None
        }
    }
}

fn print_trap(trap: &Trap, sources: &SourceMap) {
    match trap {
        Trap::Panic {
            message,
            source,
            span,
            trace,
        } => {
            if let (Some(source), Some(span)) = (source, span) {
                if let Some(file) = sources.get(*source) {
                    let (line, col) = file.line_col(span.start);
                    eprintln!("panic: {} at {}:{}:{}", message, file.name(), line, col);
                } else {
                    eprintln!("panic: {message}");
                }
            } else {
                eprintln!("panic: {message}");
            }
            for frame in trace {
                eprintln!("  in {frame}");
            }
        }
        other => eprintln!("trap: {}", other.panic_message()),
    }
}

fn load_interfaces(paths: &[PathBuf], root: Option<&Path>) -> Result<Vec<InterfaceSource>, String> {
    let mut out = Vec::new();
    for path in paths {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let relative = root
            .and_then(|root| path.strip_prefix(root).ok())
            .unwrap_or(path.as_path());
        let module = relative
            .to_string_lossy()
            .replace('\\', "/")
            .trim_start_matches("./")
            .trim_end_matches(".mti")
            .replace('/', ".");
        out.push(InterfaceSource { path: module, text });
    }
    Ok(out)
}

fn merge_interfaces(
    mut base: Vec<InterfaceSource>,
    extra: Vec<InterfaceSource>,
) -> Result<Vec<InterfaceSource>, String> {
    for source in extra {
        if let Some(existing) = base.iter().find(|existing| existing.path == source.path) {
            if existing.text != source.text {
                return Err(format!(
                    "interface module `{}` is defined more than once with different text",
                    source.path
                ));
            }
            continue;
        }
        base.push(source);
    }
    Ok(base)
}

impl StandaloneHost {
    fn register(&self, engine: &mut Engine, interfaces: &[InterfaceSource]) {
        for (path, text) in Self::MODULES {
            if interfaces.iter().any(|interface| interface.path == path) {
                continue;
            }
            engine.interface(path, text);
        }
        let script_args = self.script_args.clone();

        engine.host_fn(
            "std.io.read_line",
            HostSignature::new(
                vec![],
                HostType::Result(
                    Box::new(HostType::Option(Box::new(HostType::Str))),
                    Box::new(HostType::Str),
                ),
            ),
            |_cx, _args| {
                let mut line = String::new();
                match std::io::stdin().read_line(&mut line) {
                    Ok(0) => Ok(Value::Result(Ok(Box::new(Value::Option(None))))),
                    Ok(_) => {
                        while line.ends_with('\n') || line.ends_with('\r') {
                            line.pop();
                        }
                        Ok(Value::Result(Ok(Box::new(Value::Option(Some(Box::new(
                            Value::str(line),
                        )))))))
                    }
                    Err(error) => Ok(Value::Result(Err(Box::new(Value::str(error.to_string()))))),
                }
            },
        );
        engine.host_fn(
            "std.io.read_file",
            HostSignature::new(
                vec![HostType::Str],
                HostType::Result(Box::new(HostType::Str), Box::new(HostType::Str)),
            ),
            |_cx, args| {
                let path = args.first().and_then(|v| v.as_str()).unwrap_or("");
                match std::fs::read_to_string(path) {
                    Ok(text) => Ok(Value::Result(Ok(Box::new(Value::str(text))))),
                    Err(error) => Ok(Value::Result(Err(Box::new(Value::str(error.to_string()))))),
                }
            },
        );
        engine.host_fn(
            "std.io.write_file",
            HostSignature::new(
                vec![HostType::Str, HostType::Str],
                HostType::Result(Box::new(HostType::Unit), Box::new(HostType::Str)),
            ),
            |_cx, args| {
                let path = args.first().and_then(|v| v.as_str()).unwrap_or("");
                let text = args.get(1).and_then(|v| v.as_str()).unwrap_or("");
                match std::fs::write(path, text) {
                    Ok(()) => Ok(Value::Result(Ok(Box::new(Value::Unit)))),
                    Err(error) => Ok(Value::Result(Err(Box::new(Value::str(error.to_string()))))),
                }
            },
        );
        engine.host_fn(
            "std.env.args",
            HostSignature::new(vec![], HostType::List(Box::new(HostType::Str))),
            move |_cx, _args| {
                Ok(Value::list(
                    script_args.iter().cloned().map(Value::str).collect(),
                ))
            },
        );
        engine.host_fn(
            "std.env.get",
            HostSignature::new(
                vec![HostType::Str],
                HostType::Option(Box::new(HostType::Str)),
            ),
            |_cx, args| {
                let name = args.first().and_then(|v| v.as_str()).unwrap_or("");
                Ok(match std::env::var(name) {
                    Ok(value) => Value::Option(Some(Box::new(Value::str(value)))),
                    Err(_) => Value::Option(None),
                })
            },
        );
    }
}
