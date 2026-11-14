use std::path::{Path, PathBuf};

use martite::{HostSignature, HostType, Outcome};
use martite_base::{LoadLimits, MemoryProvider};
use martite_config::{FmtConfig, ToolConfig, parse_manifest};
use martite_driver::{LoadOptions, LoadedProgram, load};
use martite_fmt::{format_interface, format_source, structurally_equal};
use martite_ir::lower;
use martite_lint::{LintInputs, SettingsResolver, lint};
use martite_resolve::InterfaceSource;
use martite_runtime::{ExecLimits, IrRuntime, Value};
use martite_syntax::{parse_interface, parse_source, parse_source_with};

fn bench_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/bench")
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(path) else {
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

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .expect("fixture path")
        .to_string_lossy()
        .replace('\\', "/")
}

fn is_plugin_fixture(root: &Path, path: &Path) -> bool {
    let mut directory = path.parent();
    while let Some(candidate) = directory {
        if candidate.join("plugin.toml").is_file() {
            return true;
        }
        if candidate == root {
            break;
        }
        directory = candidate.parent();
    }
    false
}

fn provider_for(root: &Path) -> (MemoryProvider, Vec<InterfaceSource>, Vec<String>, String) {
    let mut provider = MemoryProvider::new();
    let mut interfaces = Vec::new();
    let mut grants = Vec::new();
    let mut entry = "main.mt".to_string();
    for path in walk(root) {
        let name = relative(root, &path);
        let text = std::fs::read_to_string(&path).expect("read fixture");
        match path.extension().and_then(|ext| ext.to_str()) {
            Some("mt") => {
                provider.insert(name, text);
            }
            Some("mti") => {
                interfaces.push(InterfaceSource {
                    path: name.trim_end_matches(".mti").replace('/', "."),
                    text,
                });
            }
            _ if name == "plugin.toml" => {
                let manifest = parse_manifest(&text).expect("manifest");
                entry = manifest.entry;
                grants = manifest.grants;
            }
            _ => {}
        }
    }
    (provider, interfaces, grants, entry)
}

fn load_fixture(name: &str) -> LoadedProgram {
    let root = bench_root().join(name);
    let (provider, interfaces, grants, entry) = provider_for(&root);
    load(
        &provider,
        &LoadOptions {
            entry,
            interfaces,
            grants,
            limits: LoadLimits::default(),
            edition: "1".into(),
            parse: martite_syntax::ParseOptions::default(),
        },
    )
}

fn load_single(path: &Path) -> LoadedProgram {
    load_single_with_parse(path, martite_syntax::ParseOptions::default())
}

fn load_single_with_parse(path: &Path, parse: martite_syntax::ParseOptions) -> LoadedProgram {
    let mut provider = MemoryProvider::new();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("fixture name");
    provider.insert(name, std::fs::read_to_string(path).expect("read fixture"));
    load(
        &provider,
        &LoadOptions {
            entry: name.into(),
            interfaces: Vec::new(),
            grants: Vec::new(),
            limits: LoadLimits::default(),
            edition: "1".into(),
            parse,
        },
    )
}

fn assert_clean(loaded: &LoadedProgram) {
    assert!(
        !loaded.has_errors(),
        "fixture failed to load: {:?}",
        loaded
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.render(&loaded.sources))
            .collect::<Vec<_>>()
    );
    assert!(loaded.program.is_some(), "fixture produced no program");
}

#[test]
fn syntax_fixtures_parse_and_format_idempotently() {
    let root = bench_root().join("syntax");
    let config = FmtConfig::default();
    for path in walk(&root) {
        let source = std::fs::read_to_string(&path).expect("read fixture");
        let name = relative(&root, &path);
        match path.extension().and_then(|ext| ext.to_str()) {
            Some("mt") => {
                let parsed = parse_source_with(
                    &source,
                    LoadLimits::default(),
                    martite_syntax::ParseOptions::TOOLING,
                );
                assert!(
                    parsed.diagnostics.is_empty(),
                    "{name} does not parse: {:?}",
                    parsed.diagnostics
                );
                let first = format_source(&source, &config);
                assert!(
                    first.diagnostics.is_empty(),
                    "{name} formatter diagnostics: {:?}",
                    first.diagnostics
                );
                let first = first.text.expect("formatted source");
                let second = format_source(&first, &config);
                assert_eq!(
                    second.text.as_deref(),
                    Some(first.as_str()),
                    "{name} is not idempotent"
                );
                let reparsed = parse_source_with(
                    &first,
                    LoadLimits::default(),
                    martite_syntax::ParseOptions::TOOLING,
                );
                assert!(
                    structurally_equal(&parsed.program, &reparsed.program),
                    "{name} changed the AST"
                );
            }
            Some("mti") => {
                let parsed = parse_interface(&source, LoadLimits::default());
                assert!(
                    parsed.diagnostics.is_empty(),
                    "{name} does not parse: {:?}",
                    parsed.diagnostics
                );
                let first = format_interface(&source, &config);
                let first = first.text.expect("formatted interface");
                let second = format_interface(&first, &config);
                assert_eq!(
                    second.text.as_deref(),
                    Some(first.as_str()),
                    "{name} is not idempotent"
                );
            }
            _ => {}
        }
    }
}

#[test]
fn parser_fixtures_cover_deep_and_malformed_input() {
    let deep = std::fs::read_to_string(bench_root().join("parser/deep_expr.mt")).expect("deep");
    let parsed = parse_source(&deep, LoadLimits::default());
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);

    let malformed =
        std::fs::read_to_string(bench_root().join("parser/malformed.mt")).expect("malformed");
    let parsed = parse_source(&malformed, LoadLimits::default());
    assert!(
        !parsed.diagnostics.is_empty(),
        "malformed fixture must produce diagnostics"
    );
}

#[test]
fn load_fixtures_type_check_cleanly() {
    for name in ["load/single", "load/multi", "load/plugin"] {
        let loaded = load_fixture(name);
        assert_clean(&loaded);
    }
}

#[test]
fn lint_fixtures_type_check_and_exercise_rules() {
    let root = bench_root().join("lint");
    for path in walk(&root) {
        let loaded = load_single_with_parse(&path, martite_syntax::ParseOptions::TOOLING);
        assert_clean(&loaded);
        let config = ToolConfig {
            lint_preset: Some("recommended".into()),
            ..ToolConfig::default()
        };
        let resolver = SettingsResolver::new(&config, None).expect("settings");
        let program = loaded.program.as_ref().expect("program");
        let output = lint(
            program,
            &loaded.sources,
            &resolver,
            "",
            &LintInputs::default(),
        );
        if path.file_name().and_then(|name| name.to_str()) == Some("noisy.mt") {
            assert!(
                !output.diagnostics.is_empty(),
                "noisy lint fixture must exercise at least one rule"
            );
        }
    }
}

#[test]
fn runtime_fixtures_run_and_plugins_round_trip_state() {
    let root = bench_root().join("runtime");
    for path in walk(&root) {
        if path.extension().and_then(|ext| ext.to_str()) != Some("mt") {
            continue;
        }
        if is_plugin_fixture(&root, &path) {
            continue;
        }
        let loaded = load_single(&path);
        assert_clean(&loaded);
        let program = loaded.program.expect("program");
        let ir = lower(&program, &loaded.sources).expect("benchmark fixture lowers");
        let mut runtime = IrRuntime::new(
            &ir,
            ExecLimits {
                fuel: 100_000_000,
                memory: 64 << 20,
                depth: 200,
                deadline: None,
            },
        );
        runtime
            .run_main()
            .unwrap_or_else(|trap| panic!("{} trapped: {trap:?}", path.display()));
    }
}

#[test]
fn runtime_plugin_fixture_handles_calls_and_snapshots() {
    let root = bench_root().join("runtime/plugin");
    let (provider, interfaces, _, _) = provider_for(&root);
    let manifest = std::fs::read_to_string(root.join("plugin.toml")).expect("manifest");
    let mut engine = martite::Engine::new();
    for interface in &interfaces {
        engine.interface(interface.path.clone(), interface.text.clone());
    }
    let plugin = engine.load(&manifest, &provider).expect("plugin loads");
    let mut instance = plugin.instantiate().expect("init");
    instance
        .call("saved", vec![Value::str("hello")])
        .expect("saved handler");
    instance.call("tick", vec![]).expect("tick handler");
    let snapshot = instance.snapshot();
    assert!(!snapshot.bytes.is_empty());

    let mut restored = plugin.instantiate().expect("init");
    restored.restore(&snapshot).expect("restore");
    restored.call("tick", vec![]).expect("tick after restore");
}

#[test]
fn runtime_host_calls_fixture_calls_host_functions() {
    let root = bench_root().join("runtime/host_calls");
    let (provider, interfaces, _, _) = provider_for(&root);
    let manifest = std::fs::read_to_string(root.join("plugin.toml")).expect("manifest");
    let mut engine = martite::Engine::new();
    for interface in &interfaces {
        engine.interface(interface.path.clone(), interface.text.clone());
    }
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
    let plugin = engine
        .load(&manifest, &provider)
        .expect("host-call plugin loads");
    let mut instance = plugin.instantiate().expect("init");
    let outcome = instance
        .call("run", vec![Value::Int(10), Value::Int(3)])
        .expect("run handler");
    assert!(
        matches!(outcome, Outcome::Handled(Value::Int(_))),
        "expected an int result, got {outcome:?}"
    );
}
