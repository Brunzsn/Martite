use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use martite::{Engine, HostSignature, HostType, LANG_EDITION};
use martite_base::{
    Applicability, Code, Diagnostic, FileSystemProvider, LoadLimits, Severity, SourceMap,
    SourceProvider, normalize_source,
};
use martite_config::{Manifest, ToolConfig, parse_manifest, parse_tool_config};
use martite_driver::{LoadOptions, load};
use martite_lint::{LintInputs, ManifestInput, SettingsResolver, normalize_path, sort_diagnostics};
use martite_resolve::InterfaceSource;
use martite_runtime::{ExecLimits, Runtime, Trap, Value};
use martite_syntax::{parse_interface, parse_source};

const USAGE: &str = "\
Martite language toolchain

Usage:
    martite <command> [arguments]

Commands:
    run <file>            Run a script
    check [options] <path> Parse and type-check
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
        "check" => cmd_check(rest, false),
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

#[derive(Default)]
struct Parsed {
    json: bool,
    interfaces: Vec<PathBuf>,
    fuel: Option<u64>,
    memory: Option<u64>,
    depth: Option<u32>,
    path: Option<String>,
    /// Arguments after the script path, passed through to `std.env.args()`.
    script_args: Vec<String>,
}

fn parse_args(args: &[String], allow_trailing: bool) -> Result<Parsed, String> {
    use lexopt::prelude::*;

    let mut out = Parsed::default();
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

fn print_rendered(diagnostics: &mut Vec<RenderedDiagnostic>, json: bool) {
    diagnostics.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then_with(|| left.start.cmp(&right.start))
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.message.cmp(&right.message))
    });
    let mut seen = HashSet::new();
    for diagnostic in diagnostics.iter() {
        let key = format!(
            "{}\0{}\0{}\0{}",
            diagnostic.file, diagnostic.start, diagnostic.code, diagnostic.message
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
        let Some(root) = find_project_root(cwd) else {
            return Err(
                "fmt requires a path when no `martite.toml` or `plugin.toml` is present".into(),
            );
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
    if args.verify {
        if let Err(message) = verify_format(source, &formatted, false) {
            eprintln!("error: formatter bug in {name}: {message}");
            return ExitCode::from(2);
        }
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
    if !martite_fmt::structurally_equal(&left.program, &right.program) {
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
    interfaces: Vec<InterfaceSource>,
    grants: Vec<String>,
    manifest: Option<Manifest>,
    manifest_text: Option<String>,
}

fn interface_paths(interfaces: &[InterfaceSource]) -> Vec<String> {
    interfaces
        .iter()
        .map(|interface| interface.path.clone())
        .collect()
}

fn script_interfaces(cli: &[PathBuf], root: &Path) -> Vec<InterfaceSource> {
    let mut interfaces = load_interfaces(cli, Some(root));
    if !interfaces
        .iter()
        .any(|interface| interface.path == "std.io")
    {
        interfaces.push(std_interface("std.io", STD_IO));
    }
    if !interfaces
        .iter()
        .any(|interface| interface.path == "std.env")
    {
        interfaces.push(std_interface("std.env", STD_ENV));
    }
    interfaces
}

fn script_spec(root: PathBuf, entry: String, cli: &[PathBuf]) -> ProjectSpec {
    let interfaces = script_interfaces(cli, &root);
    let grants = interface_paths(&interfaces);
    ProjectSpec {
        root,
        entry,
        edition: LANG_EDITION.to_string(),
        interfaces,
        grants,
        manifest: None,
        manifest_text: None,
    }
}

fn collect_projects(
    args: &LintArgs,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Vec<ProjectSpec>, String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut specs = Vec::new();

    if args.paths.is_empty() {
        let Some(root) = find_project_root(&cwd) else {
            return Err(
                "lint requires a path when no `martite.toml` or `plugin.toml` is present".into(),
            );
        };
        if root.join("plugin.toml").is_file() {
            match plugin_spec(&root, args, diagnostics) {
                Some(spec) => specs.push(spec),
                None => {}
            }
        } else {
            for file in scan_martite_files(&root) {
                let entry = relative_to(&root, &file);
                specs.push(script_spec(root.clone(), entry, &args.interfaces));
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
                specs.push(script_spec(root, entry, &args.interfaces));
            } else if path.is_dir() {
                if path.join("plugin.toml").is_file() {
                    if let Some(spec) = plugin_spec(&path, args, diagnostics) {
                        specs.push(spec);
                    }
                } else {
                    for file in scan_martite_files(&path) {
                        let entry = relative_to(&path, &file);
                        specs.push(script_spec(path.clone(), entry, &args.interfaces));
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
    args: &LintArgs,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ProjectSpec> {
    let manifest_path = root.join("plugin.toml");
    let text = match std::fs::read_to_string(&manifest_path) {
        Ok(text) => text,
        Err(error) => {
            diagnostics.push(Diagnostic::error(
                Code::E508,
                format!("cannot read {}: {error}", manifest_path.display()),
            ));
            return None;
        }
    };
    let manifest = match parse_manifest(&text) {
        Ok(manifest) => manifest,
        Err(error) => {
            diagnostics.push(Diagnostic::error(Code::E508, error.message));
            return None;
        }
    };
    let interfaces = load_interfaces(&args.interfaces, Some(root));
    Some(ProjectSpec {
        root: root.to_path_buf(),
        entry: manifest.entry.clone(),
        edition: manifest.lang.clone(),
        grants: manifest.grants.clone(),
        interfaces,
        manifest: Some(manifest),
        manifest_text: Some(text),
    })
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
    code: Code,
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
                spec.grants.clone(),
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
                interfaces: spec.interfaces.clone(),
                grants,
                limits: LoadLimits::default(),
                edition,
            },
        );
        last.clear();
        render_all(&loaded.diagnostics, &loaded.sources, &mut last);
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
        let output = martite_lint::lint(program, &loaded.sources, &resolver, &prefix, &inputs);
        render_all(&output.diagnostics, &loaded.sources, &mut last);

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
        if let Some(parent) = full.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                eprintln!("error: cannot create {}: {error}", parent.display());
                failed = true;
                continue;
            }
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
    let mut edits = Vec::new();
    for diagnostic in diagnostics {
        let Some(fix) = &diagnostic.fix else {
            continue;
        };
        if fix.applicability == Applicability::Maybe && !suggested {
            continue;
        }
        for edit in &fix.edits {
            let source = edit.source.or(diagnostic.source);
            let Some(source) = source else {
                continue;
            };
            let Some(file) = sources.get(source) else {
                continue;
            };
            edits.push(PendingEdit {
                path: file.name().to_string(),
                span: edit.span,
                replacement: edit.replacement.clone(),
                code: diagnostic.code,
            });
        }
    }
    edits.sort_by_key(|edit| (edit.span.start, edit.code, edit.span.end));
    let mut accepted: Vec<PendingEdit> = Vec::new();
    for edit in edits {
        if accepted.iter().any(|previous| interferes(previous, &edit)) {
            continue;
        }
        accepted.push(edit);
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
        let provider = FileSystemProvider::new(spec.root.clone());
        let mut loaded = load(
            &provider,
            &LoadOptions {
                entry: spec.entry.clone(),
                interfaces: spec.interfaces,
                grants: spec.grants,
                limits: LoadLimits::default(),
                edition: spec.edition,
            },
        );
        render_all(&loaded.diagnostics, &loaded.sources, &mut rendered);
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
        render_all(&output.diagnostics, &loaded.sources, &mut rendered);
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
        let Some(root) = find_project_root(&cwd) else {
            eprintln!("error: --syntax-only requires a path when no project file is present");
            return ExitCode::from(2);
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

fn cmd_run(args: &[String]) -> ExitCode {
    let parsed = match parse_args(args, true) {
        Ok(p) => p,
        Err(message) => {
            eprintln!("error: {message}\n");
            return ExitCode::from(2);
        }
    };
    let Some(file) = parsed.path.clone() else {
        eprintln!("error: run requires a file\n");
        return ExitCode::from(2);
    };
    let path = PathBuf::from(&file);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        eprintln!("error: invalid file path `{file}`");
        return ExitCode::from(2);
    };
    if !path.is_file() {
        eprintln!("error: file not found: {file}");
        return ExitCode::from(2);
    }
    // `martite run` is the standalone host: it is intentionally unrestricted.
    let provider = FileSystemProvider::unrestricted(parent);
    let mut engine = Engine::new();
    register_std(&mut engine, parsed.script_args.clone());
    apply_limits(&mut engine, &parsed);
    let plugin = match engine.load_script(name, &provider) {
        Ok(plugin) => plugin,
        Err(diagnostics) => {
            print_diagnostics(&diagnostics, &SourceMap::new(), parsed.json);
            return ExitCode::from(2);
        }
    };
    let result = plugin.run_main();
    match result {
        Ok(main) => {
            for line in main.log {
                println!("{line}");
            }
            match main.value {
                Value::Result(Err(error)) => {
                    eprintln!(
                        "{}",
                        martite_runtime::show(&error, &plugin.program().types, true)
                    );
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

fn cmd_check(args: &[String], lint: bool) -> ExitCode {
    let parsed = match parse_args(args, false) {
        Ok(p) => p,
        Err(message) => {
            eprintln!("error: {message}\n");
            return ExitCode::from(2);
        }
    };
    let Some(path) = parsed.path else {
        eprintln!(
            "error: {} requires a path",
            if lint { "lint" } else { "check" }
        );
        return ExitCode::from(2);
    };
    let path = PathBuf::from(&path);
    let (root, entry, edition, load_limits, mut interfaces, mut grants) = if path.is_dir() {
        let manifest_path = path.join("plugin.toml");
        let manifest_text = match std::fs::read_to_string(&manifest_path) {
            Ok(text) => text,
            Err(error) => {
                eprintln!("error: cannot read {}: {error}", manifest_path.display());
                return ExitCode::from(2);
            }
        };
        let manifest = match parse_manifest(&manifest_text) {
            Ok(m) => m,
            Err(error) => {
                let d = Diagnostic::error(Code::E508, error.to_string());
                print_diagnostics(&[d], &SourceMap::new(), parsed.json);
                return ExitCode::from(1);
            }
        };
        let interfaces = load_interfaces(&parsed.interfaces, Some(&path));
        (
            path.clone(),
            manifest.entry.clone(),
            manifest.lang.clone(),
            LoadLimits::default(),
            interfaces,
            manifest.grants.clone(),
        )
    } else if path.is_file() {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_path_buf();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("main.mt")
            .to_string();
        let interfaces = load_interfaces(&parsed.interfaces, Some(&parent));
        (
            parent,
            name,
            LANG_EDITION.to_string(),
            LoadLimits::default(),
            interfaces,
            Vec::new(),
        )
    } else {
        eprintln!("error: path not found: {}", path.display());
        return ExitCode::from(2);
    };
    if !path.is_dir() {
        if !interfaces.iter().any(|i| i.path == "std.io") {
            interfaces.push(std_interface("std.io", STD_IO));
        }
        if !interfaces.iter().any(|i| i.path == "std.env") {
            interfaces.push(std_interface("std.env", STD_ENV));
        }
        grants = interfaces.iter().map(|i| i.path.clone()).collect();
    }
    let provider = FileSystemProvider::new(root);
    let loaded = load(
        &provider,
        &LoadOptions {
            entry,
            interfaces,
            grants,
            limits: load_limits,
            edition,
        },
    );
    print_diagnostics(&loaded.diagnostics, &loaded.sources, parsed.json);
    let has_errors = loaded
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error);
    if has_errors {
        ExitCode::from(1)
    } else if lint && loaded.diagnostics.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::SUCCESS
    }
}

fn cmd_test(args: &[String]) -> ExitCode {
    let parsed = match parse_args(args, false) {
        Ok(p) => p,
        Err(message) => {
            eprintln!("error: {message}\n");
            return ExitCode::from(2);
        }
    };
    let Some(path) = parsed.path else {
        eprintln!("error: test requires a path\n");
        return ExitCode::from(2);
    };
    let path = PathBuf::from(&path);
    let (root, entry, mut interfaces, mut grants) = if path.is_dir() {
        let manifest_path = path.join("plugin.toml");
        let manifest = match std::fs::read_to_string(&manifest_path)
            .ok()
            .and_then(|text| parse_manifest(&text).ok())
        {
            Some(m) => m,
            None => {
                eprintln!("error: cannot load {}", manifest_path.display());
                return ExitCode::from(2);
            }
        };
        let interfaces = load_interfaces(&parsed.interfaces, Some(&path));
        (
            path.clone(),
            manifest.entry.clone(),
            interfaces,
            manifest.grants.clone(),
        )
    } else if path.is_file() {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_path_buf();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("main.mt")
            .to_string();
        let interfaces = load_interfaces(&parsed.interfaces, Some(&parent));
        (parent, name, interfaces, Vec::new())
    } else {
        eprintln!("error: path not found: {}", path.display());
        return ExitCode::from(2);
    };
    if !path.is_dir() {
        if !interfaces.iter().any(|i| i.path == "std.io") {
            interfaces.push(std_interface("std.io", STD_IO));
        }
        if !interfaces.iter().any(|i| i.path == "std.env") {
            interfaces.push(std_interface("std.env", STD_ENV));
        }
        grants = interfaces.iter().map(|i| i.path.clone()).collect();
    }
    let provider = FileSystemProvider::new(root);
    let loaded = load(
        &provider,
        &LoadOptions {
            entry,
            interfaces,
            grants,
            limits: LoadLimits::default(),
            edition: LANG_EDITION.into(),
        },
    );
    if loaded.has_errors() {
        print_diagnostics(&loaded.diagnostics, &loaded.sources, parsed.json);
        return ExitCode::from(1);
    }
    let Some(program) = loaded.program else {
        eprintln!("error: no program");
        return ExitCode::from(2);
    };
    let limits = ExecLimits {
        fuel: parsed.fuel.unwrap_or(1_000_000),
        memory: parsed.memory.unwrap_or(8 << 20),
        depth: parsed.depth.unwrap_or(200),
        deadline: None,
    };
    let mut failed = 0usize;
    let mut total = 0usize;
    for def in &program.tests {
        total += 1;
        let name = program.resolved.def(*def).name.clone();
        let mut runtime = Runtime::new(&program, limits);
        match runtime.invoke(*def, Vec::new()) {
            Ok(result) => {
                for line in runtime.take_log() {
                    println!("{line}");
                }
                println!("test {name} ... ok");
                let _ = result;
            }
            Err(trap) => {
                for line in runtime.take_log() {
                    println!("{line}");
                }
                failed += 1;
                println!("test {name} ... FAILED: {}", trap.panic_message());
            }
        }
    }
    if total == 0 {
        println!("no tests found");
    } else {
        println!("{}/{} tests passed", total - failed, total);
    }
    if failed > 0 {
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

fn load_interfaces(paths: &[PathBuf], root: Option<&Path>) -> Vec<InterfaceSource> {
    let mut out = Vec::new();
    for path in paths {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                eprintln!("error: cannot read {}: {error}", path.display());
                continue;
            }
        };
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
    out
}

fn std_interface(path: &str, text: &str) -> InterfaceSource {
    InterfaceSource {
        path: path.into(),
        text: text.into(),
    }
}

#[allow(clippy::too_many_arguments)]
fn register_std(engine: &mut Engine, args: Vec<String>) {
    engine.interface("std.io", STD_IO);
    engine.interface("std.env", STD_ENV);

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
                args.clone().into_iter().map(Value::str).collect(),
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

fn apply_limits(engine: &mut Engine, parsed: &Parsed) {
    let limits = ExecLimits {
        fuel: parsed.fuel.unwrap_or(1_000_000),
        memory: parsed.memory.unwrap_or(8 << 20),
        depth: parsed.depth.unwrap_or(200),
        deadline: None,
    };
    engine.set_limits(limits, limits);
}
