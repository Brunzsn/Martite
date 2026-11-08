//! Shared fixture loading and process setup for the Martite benchmarks.
//!
//! The benchmark corpus lives in `martite-conformance/fixtures/bench` so the
//! conformance suite can validate the same files the benches time.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use martite::{Engine, HostSignature, HostType, Plugin};
use martite_base::{LoadLimits, MemoryProvider, SourceMap};
use martite_check::CheckedProgram;
use martite_config::{Manifest, parse_manifest};
use martite_driver::{LoadOptions, LoadedProgram, load};
use martite_resolve::InterfaceSource;
use martite_runtime::{ExecLimits, Value};
use martite_syntax::ParseOptions;

/// Generous execution limits so benchmark fixtures never trap.
pub const BENCH_LIMITS: ExecLimits = ExecLimits {
    fuel: 100_000_000,
    memory: 64 << 20,
    depth: 200,
    deadline: None,
};

/// Number of host calls the `runtime/host_calls` fixture performs per run.
pub const HOST_CALLS_COUNT: i64 = 1000;

/// Root of the deterministic benchmark corpus owned by the conformance suite.
pub fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../martite-conformance/fixtures/bench")
        .canonicalize()
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../martite-conformance/fixtures/bench")
                .to_path_buf()
        })
}

/// Workspace root, derived from this crate's manifest directory.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

/// Cargo target directory, honoring `CARGO_TARGET_DIR`.
pub fn target_dir() -> PathBuf {
    let root = workspace_root();
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(value) => {
            let target = PathBuf::from(value);
            if target.is_absolute() {
                target
            } else {
                root.join(target)
            }
        }
        None => root.join("target"),
    }
}

/// Directory where Criterion writes its JSON and HTML output.
pub fn criterion_dir() -> PathBuf {
    target_dir().join("criterion")
}

/// Root of the formatter golden fixtures.
pub fn formatter_corpus_root() -> PathBuf {
    workspace_root().join("crates/martite-fmt/tests/fixtures")
}

/// Path of a corpus entry, which may be a single file or a directory.
pub fn fixture_path(name: &str) -> PathBuf {
    corpus_root().join(name)
}

/// Read one fixture file as text.
pub fn read_fixture(name: &str) -> String {
    let path = fixture_path(name);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// Recursively collect files below `root`, sorted for determinism.
pub fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        let mut entries: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
        entries.sort();
        for entry in entries {
            if entry.is_dir() {
                pending.push(entry);
            } else {
                out.push(entry);
            }
        }
    }
    out.sort();
    out
}

/// A loaded benchmark fixture.
#[derive(Debug, Clone)]
pub struct Fixture {
    pub name: String,
    pub root: PathBuf,
    /// Module files keyed by forward-slash relative path.
    pub files: Vec<(String, String)>,
    pub interfaces: Vec<InterfaceSource>,
    pub grants: Vec<String>,
    pub entry: String,
    pub manifest_text: Option<String>,
    pub manifest: Option<Manifest>,
}

impl Fixture {
    pub fn source(&self, path: &str) -> &str {
        self.files
            .iter()
            .find(|(name, _)| name == path)
            .map(|(_, text)| text.as_str())
            .unwrap_or_else(|| panic!("fixture `{}` has no file `{path}`", self.name))
    }

    pub fn provider(&self) -> MemoryProvider {
        let mut provider = MemoryProvider::new();
        for (path, text) in &self.files {
            provider.insert(path.clone(), text.clone());
        }
        provider
    }

    pub fn load_options(&self) -> LoadOptions {
        self.load_options_with_parse(ParseOptions::default())
    }

    pub fn load_options_with_parse(&self, parse: ParseOptions) -> LoadOptions {
        LoadOptions {
            entry: self.entry.clone(),
            interfaces: self.interfaces.clone(),
            grants: self.grants.clone(),
            limits: LoadLimits::default(),
            edition: "1".into(),
            parse,
        }
    }

    pub fn load(&self) -> LoadedProgram {
        load(&self.provider(), &self.load_options())
    }

    /// Load and check a fixture, panicking if it has compile errors.
    pub fn checked(&self) -> (CheckedProgram, SourceMap) {
        self.checked_with_parse(ParseOptions::default())
    }

    /// Load and check a fixture with explicit parser options.
    pub fn checked_with_parse(&self, parse: ParseOptions) -> (CheckedProgram, SourceMap) {
        let loaded = load(&self.provider(), &self.load_options_with_parse(parse));
        if let Some(error) = loaded
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.severity == martite_base::Severity::Error)
        {
            panic!(
                "fixture `{}` does not check: {}",
                self.name,
                error.render(&loaded.sources)
            );
        }
        let program = loaded
            .program
            .unwrap_or_else(|| panic!("fixture `{}` has no program", self.name));
        (program, loaded.sources)
    }

    pub fn engine(&self) -> Engine {
        let mut engine = Engine::new();
        for interface in &self.interfaces {
            engine.interface(interface.path.clone(), interface.text.clone());
        }
        engine
    }

    pub fn load_plugin(&self) -> Plugin {
        self.load_plugin_with(&self.engine())
    }

    pub fn load_plugin_with(&self, engine: &Engine) -> Plugin {
        let manifest = self
            .manifest_text
            .as_deref()
            .unwrap_or_else(|| panic!("fixture `{}` has no plugin.toml", self.name));
        let provider = self.provider();
        engine
            .load(manifest, &provider)
            .unwrap_or_else(|diagnostics| {
                panic!(
                    "fixture `{}` failed to load: {} diagnostics",
                    self.name,
                    diagnostics.len()
                )
            })
    }
}

/// Load a corpus file or directory.
pub fn fixture(name: &str) -> Fixture {
    let root = fixture_path(name);
    let mut files = Vec::new();
    let mut interfaces = Vec::new();
    let mut manifest_text = None;
    let mut manifest = None;
    let mut entry = "main.mt".to_string();

    if root.is_file() {
        let file_name = root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("main.mt")
            .to_string();
        let text = fs::read_to_string(&root)
            .unwrap_or_else(|error| panic!("read {}: {error}", root.display()));
        if root.extension().and_then(|ext| ext.to_str()) == Some("mti") {
            let interface = file_name.trim_end_matches(".mti").replace('/', ".");
            interfaces.push(InterfaceSource {
                path: interface,
                text,
            });
        } else {
            entry = file_name.clone();
            files.push((file_name, text));
        }
    } else {
        for path in walk_files(&root) {
            let relative = path
                .strip_prefix(&root)
                .expect("fixture path")
                .to_string_lossy()
                .replace('\\', "/");
            let text = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            match path.extension().and_then(|ext| ext.to_str()) {
                Some("mt") => files.push((relative, text)),
                Some("mti") => {
                    let interface = relative.trim_end_matches(".mti").replace('/', ".");
                    interfaces.push(InterfaceSource {
                        path: interface,
                        text,
                    });
                }
                _ if relative == "plugin.toml" => {
                    let parsed = parse_manifest(&text)
                        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
                    entry = parsed.entry.clone();
                    manifest_text = Some(text);
                    manifest = Some(parsed);
                }
                _ => {}
            }
        }
    }

    let grants = manifest
        .as_ref()
        .map(|manifest| manifest.grants.clone())
        .unwrap_or_default();

    Fixture {
        name: name.to_string(),
        root,
        files,
        interfaces,
        grants,
        entry,
        manifest_text,
        manifest,
    }
}

/// Load the `runtime/host_calls` fixture with deterministic host functions.
///
/// The fixture loops over `app.bench.mix`, passing an opaque `Token` handle
/// created by `app.bench.mint`, so the benchmark covers both host-call dispatch
/// and handle copies.
pub fn host_calls_plugin() -> Plugin {
    let fixture = fixture("runtime/host_calls");
    let mut engine = fixture.engine();
    engine.host_fn(
        "app.bench.mint",
        HostSignature::new(vec![HostType::Int], HostType::Handle("Token".into())),
        |cx, args| {
            let seed = args.first().and_then(Value::as_int).unwrap_or(0);
            cx.make_handle(seed.to_string())
        },
    );
    engine.host_fn(
        "app.bench.mix",
        HostSignature::new(
            vec![HostType::Handle("Token".into()), HostType::Int],
            HostType::Int,
        ),
        |_cx, args| {
            let value = args.get(1).and_then(Value::as_int).unwrap_or(0);
            Ok(Value::Int(value.wrapping_mul(31).wrapping_add(7) & 0xffff))
        },
    );
    fixture.load_plugin_with(&engine)
}

/// Locate the `martite` CLI binary used by the startup benchmarks.
///
/// The binary is built outside the normal target directory as a last resort so
/// a nested `cargo build` cannot contend with the `cargo bench` build lock.
pub fn cli_binary() -> PathBuf {
    let exe = if cfg!(windows) {
        "martite.exe"
    } else {
        "martite"
    };
    let root = workspace_root();

    if let Some(value) = std::env::var_os("MARTITE_BIN") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return path;
        }
    }

    let target = target_dir();
    let mut candidates = vec![
        target.join("release").join(exe),
        target.join("debug").join(exe),
        target.join("bench-cli/release").join(exe),
    ];
    if target != root.join("target") {
        candidates.push(root.join("target/release").join(exe));
        candidates.push(root.join("target/debug").join(exe));
    }

    for candidate in &candidates {
        if candidate.is_file() {
            return candidate.clone();
        }
    }

    let target_dir = target.join("bench-cli");
    eprintln!(
        "bench: building martite-cli into {} for the CLI startup group",
        target_dir.display()
    );
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let status = Command::new(cargo)
        .args(["build", "--release", "--quiet", "-p", "martite-cli"])
        .arg("--target-dir")
        .arg(&target_dir)
        .status()
        .expect("run cargo build for martite-cli");
    assert!(status.success(), "failed to build martite-cli");

    let path = target_dir.join("release").join(exe);
    assert!(
        path.is_file(),
        "martite-cli was built but {} is missing",
        path.display()
    );
    path
}
