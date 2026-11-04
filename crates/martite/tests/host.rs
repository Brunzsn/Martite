use martite::{Engine, HostSignature, HostType, Instance, Outcome};
use martite_base::MemoryProvider;
use martite_runtime::Value;

const INTERFACE: &str = r#"struct Doc
fn text(doc: Doc) -> str
on saved(doc: Doc) -> Result[(), str]
"#;

const MANIFEST: &str = r#"
[plugin]
name = "word-count"
version = "0.1.0"
lang = "1"
api = "1.0"
entry = "main.mt"

[permissions]
grant = ["app.editor"]
"#;

const PLUGIN: &str = r#"
use app.editor
use app.editor.Doc

struct Stats { saves: int, last_len: int }

on init() -> Stats { Stats { saves: 0, last_len: 0 } }

on saved(mut self, doc: Doc) -> Result[(), str] {
    let words = editor.text(doc).len()
    self.saves += 1
    self.last_len = words
    Ok(())
}
"#;

fn editor_engine() -> Engine {
    let mut engine = Engine::new();
    engine.interface("app.editor", INTERFACE);
    engine.host_fn(
        "app.editor.text",
        HostSignature::new(vec![HostType::Handle("Doc".into())], HostType::Str),
        |_cx, _args| Ok(Value::str("hello world")),
    );
    engine
}

fn load_plugin() -> martite::Plugin {
    let engine = editor_engine();
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", PLUGIN);
    engine
        .load(MANIFEST, &provider)
        .unwrap_or_else(|diagnostics| {
            let sources = martite_base::SourceMap::new();
            panic!(
                "{}",
                diagnostics
                    .iter()
                    .map(|d| d.render(&sources))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        })
}

fn state_saves(instance: &Instance<'_>) -> i64 {
    let Some(Value::Struct(state)) = instance.state() else {
        panic!("expected struct state");
    };
    let Value::Int(saves) = &state.fields[0] else {
        panic!("expected saves field");
    };
    *saves
}

#[test]
fn plugin_init_hook_and_state() {
    let plugin = load_plugin();
    let doc = plugin.make_handle("Doc", "doc-1").expect("handle");
    let mut instance: Instance<'_> = plugin.instantiate().expect("init");
    match instance.call("saved", vec![doc]).expect("hook") {
        Outcome::Handled(value) => {
            assert!(
                matches!(value, Value::Result(Ok(_))),
                "unexpected {value:?}"
            );
        }
        Outcome::NoHandler => panic!("handler missing"),
    }
    assert!(matches!(instance.state(), Some(Value::Struct(_))));
    let snapshot = instance.snapshot();
    assert_ne!(snapshot.type_fingerprint, [0u8; 32]);
    assert!(!snapshot.bytes.is_empty());

    let mut restored = plugin.instantiate().expect("init");
    restored.restore(&snapshot).expect("restore");
    let before = martite_runtime::show(instance.state().unwrap(), &plugin.program().types, false);
    let after = martite_runtime::show(restored.state().unwrap(), &plugin.program().types, false);
    assert_eq!(before, after);
}

#[test]
fn reload_keeps_snapshot_when_the_state_type_is_unchanged() {
    let engine = editor_engine();
    let plugin = load_plugin();
    let doc = plugin.make_handle("Doc", "doc-1").unwrap();
    let mut instance = plugin.instantiate().unwrap();
    instance.call("saved", vec![doc]).unwrap();
    assert_eq!(state_saves(&instance), 1);
    let snapshot = instance.snapshot();

    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", PLUGIN);
    let reloaded = engine
        .reload(MANIFEST, &provider, &plugin, &snapshot)
        .expect("reload");
    assert!(reloaded.snapshot_kept);
    let restored = reloaded.instantiate().expect("restore");
    assert_eq!(state_saves(&restored), 1, "init must not reset the state");
}

#[test]
fn reload_reinitializes_when_the_state_type_changes() {
    let engine = editor_engine();
    let plugin = load_plugin();
    let doc = plugin.make_handle("Doc", "doc-1").unwrap();
    let mut instance = plugin.instantiate().unwrap();
    instance.call("saved", vec![doc]).unwrap();
    assert_eq!(state_saves(&instance), 1);
    let snapshot = instance.snapshot();

    let changed = PLUGIN
        .replace(
            "struct Stats { saves: int, last_len: int }",
            "struct Stats { saves: int, last_len: int, extra: int }",
        )
        .replace(
            "Stats { saves: 0, last_len: 0 }",
            "Stats { saves: 0, last_len: 0, extra: 0 }",
        );
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", changed);
    let reloaded = engine
        .reload(MANIFEST, &provider, &plugin, &snapshot)
        .expect("reload");
    assert!(!reloaded.snapshot_kept);
    let reset = reloaded.instantiate().expect("init");
    assert_eq!(state_saves(&reset), 0, "init must run after a type change");
}

const MATH_INTERFACE: &str = "fn answer() -> int\n";
const MATH_MANIFEST: &str = r#"
[plugin]
name = "math-tests"
version = "0.1.0"
lang = "1"
api = "1.0"
entry = "main.mt"

[permissions]
grant = ["app.math"]
"#;
const MATH_PLUGIN: &str = r#"
use app.math

test "test double answers" {
    assert(math.answer() == 42, "fake host function returned the wrong value")
}
"#;

#[test]
fn test_doubles_serve_offline_tests() {
    let mut engine = Engine::new();
    engine.interface("app.math", MATH_INTERFACE);
    engine.test_double_fn(
        "app.math.answer",
        HostSignature::new(vec![], HostType::Int),
        |_cx, _args| Ok(Value::Int(42)),
    );
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", MATH_PLUGIN);
    let plugin = engine.load(MATH_MANIFEST, &provider).expect("load");
    let reports = plugin.run_tests();
    assert_eq!(reports.len(), 1);
    assert!(reports[0].passed(), "test failed: {:?}", reports[0].outcome);
}

#[test]
fn missing_host_implementation_is_a_load_error() {
    let mut engine = Engine::new();
    engine.interface("app.editor", INTERFACE);
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", PLUGIN);
    let error = match engine.load(MANIFEST, &provider) {
        Ok(_) => panic!("expected a load error"),
        Err(error) => error,
    };
    assert!(error.iter().any(
        |d| d.code == martite_base::Code::E509 && d.message.contains("no Rust implementation")
    ));
}

const SETTINGS_PLUGIN: &str = r#"
use std.json
use std.json.Json

struct Stats { text: str }

on init(settings: Json) -> Stats {
    Stats { text: json.stringify(settings) }
}
"#;

#[test]
fn settings_are_delivered_as_json() {
    let engine = Engine::new();
    let manifest = r#"
[plugin]
name = "settings-test"
version = "0.1.0"
lang = "1"
api = "1.0"
entry = "main.mt"

[settings]
max_words = 5000
enabled = true
"#;
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", SETTINGS_PLUGIN);
    let plugin = engine.load(manifest, &provider).expect("load");
    let instance = plugin.instantiate().expect("init");
    let Some(Value::Struct(state)) = instance.state() else {
        panic!("expected struct state");
    };
    let Value::Str(text) = &state.fields[0] else {
        panic!("expected string");
    };
    assert!(text.contains("\"max_words\":5000.0"));
    assert!(text.contains("\"enabled\":true"));
}

#[test]
fn mismatched_host_signature_is_a_load_error() {
    let mut engine = Engine::new();
    engine.interface("app.editor", INTERFACE);
    engine.host_fn(
        "app.editor.text",
        HostSignature::new(vec![HostType::Str], HostType::Str),
        |_cx, _args| Ok(Value::str("x")),
    );
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", PLUGIN);
    let error = match engine.load(MANIFEST, &provider) {
        Ok(_) => panic!("expected a load error"),
        Err(error) => error,
    };
    assert!(
        error.iter().any(|d| d.code == martite_base::Code::E509
            && d.message.contains("mismatched Rust signature"))
    );
}

#[test]
fn api_major_mismatch_is_e508() {
    let mut engine = Engine::new();
    engine.interface("app.editor", INTERFACE);
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", PLUGIN);
    let bad_manifest = MANIFEST.replace("api = \"1.0\"", "api = \"2.0\"");
    let error = match engine.load(&bad_manifest, &provider) {
        Ok(_) => panic!("expected a load error"),
        Err(error) => error,
    };
    assert!(error.iter().any(|d| d.code == martite_base::Code::E508));
}

#[test]
fn ungranted_host_module_is_e501() {
    let mut engine = Engine::new();
    engine.interface("app.editor", INTERFACE);
    engine.host_fn(
        "app.editor.text",
        HostSignature::new(vec![HostType::Handle("Doc".into())], HostType::Str),
        |_cx, _args| Ok(Value::str("x")),
    );
    let manifest = MANIFEST.replace("grant = [\"app.editor\"]", "grant = []");
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", PLUGIN);
    let error = match engine.load(&manifest, &provider) {
        Ok(_) => panic!("expected a load error"),
        Err(error) => error,
    };
    assert!(error.iter().any(|d| d.code == martite_base::Code::E501));
}
