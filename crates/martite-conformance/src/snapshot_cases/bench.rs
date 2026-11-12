use std::path::{Path, PathBuf};

use martite::{HostSignature, HostType};
use martite_base::LoadLimits;
use martite_resolve::InterfaceSource;
use martite_runtime::Value;

use crate::snapshot::{ArgSpec, HostSet, PluginStep, SnapshotCase};
use crate::snapshot_cases::{bench_caps, plugin_case};

pub fn cases() -> Vec<SnapshotCase> {
    let root = fixture_root();
    let mut cases = Vec::new();
    if let Some(case) = main_dir_case("single", &root.join("load/single")) {
        cases.push(case);
    }
    if let Some(case) = main_dir_case("multi", &root.join("load/multi")) {
        cases.push(case);
    }
    for path in walk(&root.join("runtime")) {
        if path.extension().and_then(|ext| ext.to_str()) != Some("mt") {
            continue;
        }
        if is_plugin_file(&root.join("runtime"), &path) {
            continue;
        }
        let name = relative(&root, &path);
        let source = std::fs::read_to_string(&path).expect("read runtime fixture");
        cases.push(super::main_case(
            format!("bench/{name}"),
            source,
            None,
            bench_caps(),
        ));
    }
    cases.push(plugin_runtime());
    cases.push(host_calls());
    cases.push(load_plugin());
    cases
}

fn fixture_root() -> PathBuf {
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

fn is_plugin_file(root: &Path, path: &Path) -> bool {
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

fn main_dir_case(name: &str, root: &Path) -> Option<SnapshotCase> {
    let mut files = Vec::new();
    let mut interfaces = Vec::new();
    let mut entry = "main.mt".to_string();
    for path in walk(root) {
        let name = relative(root, &path);
        let text = std::fs::read_to_string(&path).expect("read fixture");
        match path.extension().and_then(|ext| ext.to_str()) {
            Some("mt") => {
                files.push((name, text));
            }
            Some("mti") => interfaces.push(InterfaceSource {
                path: name.trim_end_matches(".mti").replace('/', "."),
                text,
            }),
            _ if name == "plugin.toml" => {
                let manifest = martite_config::parse_manifest(&text).expect("manifest");
                entry = manifest.entry;
            }
            _ => {}
        }
    }
    if !files.iter().any(|(_, text)| text.contains("fn main(")) {
        return None;
    }
    Some(SnapshotCase::Main {
        id: format!("bench/load/{name}"),
        case: crate::Case {
            files,
            entry,
            interfaces,
            grants: Vec::new(),
            limits: LoadLimits::default(),
            edition: "1".into(),
        },
        hosts: None,
        caps: bench_caps(),
    })
}

fn plugin_runtime() -> SnapshotCase {
    let root = fixture_root().join("runtime/plugin");
    let files = fixture_files(&root);
    let manifest = std::fs::read_to_string(root.join("plugin.toml")).expect("manifest");
    plugin_case(
        "bench/runtime/plugin",
        manifest,
        files,
        None,
        vec![
            PluginStep::Init,
            PluginStep::Call {
                hook: "saved",
                args: vec![ArgSpec::str("benchmark")],
            },
            PluginStep::Call {
                hook: "tick",
                args: Vec::new(),
            },
        ],
        bench_caps(),
    )
}

fn host_calls() -> SnapshotCase {
    let root = fixture_root().join("runtime/host_calls");
    let files = fixture_files(&root);
    let manifest = std::fs::read_to_string(root.join("plugin.toml")).expect("manifest");
    let mut hosts = HostSet::new();
    hosts.add(
        "app.bench.mint",
        HostSignature::new(vec![HostType::Int], HostType::Handle("Token".into())),
        |cx, args| {
            let seed = args.first().and_then(Value::as_int).unwrap_or(0);
            cx.make_handle(seed.to_string())
        },
    );
    hosts.add(
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
    plugin_case(
        "bench/runtime/host_calls",
        manifest,
        files,
        Some(hosts),
        vec![
            PluginStep::Init,
            PluginStep::Call {
                hook: "run",
                args: vec![ArgSpec::int(10), ArgSpec::int(7)],
            },
        ],
        bench_caps(),
    )
}

fn load_plugin() -> SnapshotCase {
    let root = fixture_root().join("load/plugin");
    let files = fixture_files(&root);
    let manifest = std::fs::read_to_string(root.join("plugin.toml")).expect("manifest");
    let mut hosts = HostSet::new();
    hosts.add(
        "app.editor.text",
        HostSignature::new(vec![HostType::Handle("Doc".into())], HostType::Str),
        |_cx, _args| Ok(Value::str("hello")),
    );
    plugin_case(
        "bench/load/plugin",
        manifest,
        files,
        Some(hosts),
        vec![
            PluginStep::Init,
            PluginStep::Call {
                hook: "saved",
                args: vec![ArgSpec::handle("Doc", "ok")],
            },
        ],
        bench_caps(),
    )
}

fn fixture_files(root: &Path) -> Vec<(String, String)> {
    let mut files = Vec::new();
    for path in walk(root) {
        let name = relative(root, &path);
        if name == "plugin.toml" {
            continue;
        }
        if matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("mt") | Some("mti")
        ) {
            files.push((
                name,
                std::fs::read_to_string(&path).expect("read fixture file"),
            ));
        }
    }
    files
}
