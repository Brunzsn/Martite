use std::path::{Path, PathBuf};
use std::process::ExitCode;

use martite::{Engine, HostSignature, HostType, LANG_EDITION};
use martite_base::{Code, Diagnostic, FileSystemProvider, LoadLimits, Severity, SourceMap};
use martite_config::parse_manifest;
use martite_driver::{LoadOptions, load};
use martite_resolve::InterfaceSource;
use martite_runtime::{ExecLimits, Runtime, Trap, Value};

const USAGE: &str = "\
Martite language toolchain

Usage:
    martite <command> [arguments]

Commands:
    run <file>            Run a script
    check [options] <path> Parse and type-check
    lint [options] <path> Lint a project
    test [options] <path> Run test blocks
    fmt [options] <path>  Format sources
    help                  Show this help
    version               Show the toolchain version

Options:
    --json                Emit JSON diagnostics
    --interface <file>    Add a host interface (.mti)
    --fuel <n>            Fuel for `run` and `test`
    --memory <n>          Memory limit in bytes for `run` and `test`
    --depth <n>           Call-depth limit for `run` and `test`
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
        "lint" => {
            eprintln!("martite lint: the linter is not available yet");
            ExitCode::from(2)
        }
        "test" => cmd_test(rest),
        "fmt" => {
            eprintln!("martite fmt: the formatter is not available yet");
            ExitCode::from(2)
        }
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
                return Err("--fix is not supported; the linter is not implemented".into());
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
