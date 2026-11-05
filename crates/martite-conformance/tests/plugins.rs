use martite::{Engine, HostSignature, HostType, Instance, Limits, Outcome};
use martite_base::{Code, Diagnostic, MemoryProvider};
use martite_runtime::{HostContext, HostFault, Value};

const INTERFACE: &str = r#"struct Doc
fn text(doc: Doc) -> str
fn maybe() -> str?
fn stale_text(doc: Doc) -> str?
on saved(doc: Doc) -> Result[(), str]
on lookup(doc: Doc) -> Result[(), str]
on tick()
on fail() -> Result[(), str]
on boom()
on mutate(x: int) -> int
on check()
"#;

const MANIFEST: &str = r#"
[plugin]
name = "conformance-plugin"
version = "0.1.0"
lang = "1"
api = "1.0"
entry = "main.mt"

[permissions]
grant = ["app.editor"]

[limits]
fuel = 100000
memory = 1048576
depth = 100
"#;

const PLUGIN: &str = r#"
use app.editor
use app.editor.Doc

struct State { saves: int, text: str }

on init() -> State { State { saves: 0, text: "" } }

on saved(mut self, doc: Doc) -> Result[(), str] {
    self.text = editor.text(doc)
    self.saves += 1
    Ok(())
}

on lookup(mut self, doc: Doc) -> Result[(), str] {
    match editor.stale_text(doc) {
        Some(value) => {
            self.text = value
            Ok(())
        }
        None => Err("stale handle"),
    }
}

on tick(mut self) { self.saves += 1 }

on fail(mut self) -> Result[(), str] {
    self.saves += 1
    Err("failed")
}

on boom(mut self) {
    self.saves += 1
    panic("boom")
}

on mutate(mut x: int) -> int {
    x += 1
    x
}

on shutdown(self) {}
"#;

fn editor_engine() -> Engine {
    let mut engine = Engine::new();
    engine.interface("app.editor", INTERFACE);
    engine.host_fn(
        "app.editor.text",
        HostSignature::new(vec![HostType::Handle("Doc".into())], HostType::Str),
        |_cx, args| match args.first() {
            Some(Value::Handle(handle)) if handle.token == "ok" => Ok(Value::str("hello")),
            Some(Value::Handle(_)) => Err(HostFault::new("stale handle")),
            _ => Err(HostFault::new("expected a Doc handle")),
        },
    );
    engine.host_fn(
        "app.editor.maybe",
        HostSignature::new(vec![], HostType::Option(Box::new(HostType::Str))),
        |_cx, _args| Ok(Value::Option(Some(Box::new(Value::str("maybe"))))),
    );
    engine.host_fn(
        "app.editor.stale_text",
        HostSignature::new(
            vec![HostType::Handle("Doc".into())],
            HostType::Option(Box::new(HostType::Str)),
        ),
        |_cx, args| match args.first() {
            Some(Value::Handle(handle)) if handle.token == "ok" => {
                Ok(Value::Option(Some(Box::new(Value::str("hello")))))
            }
            Some(Value::Handle(_)) => Ok(Value::Option(None)),
            _ => Err(HostFault::new("expected a Doc handle")),
        },
    );
    engine
}

fn load(source: &str, manifest: &str) -> Result<martite::Plugin, Vec<Diagnostic>> {
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", source);
    editor_engine().load(manifest, &provider)
}

fn state<'a, 'p>(instance: &'a Instance<'p>) -> &'a Value {
    instance.state().expect("stateful plugin")
}

fn state_int(instance: &Instance<'_>, index: usize) -> i64 {
    let Value::Struct(state) = state(instance) else {
        panic!("expected struct state");
    };
    let Value::Int(value) = &state.fields[index] else {
        panic!("expected int field");
    };
    *value
}

fn state_str<'a, 'p>(instance: &'a Instance<'p>, index: usize) -> &'a str {
    let Value::Struct(state) = state(instance) else {
        panic!("expected struct state");
    };
    let Value::Str(value) = &state.fields[index] else {
        panic!("expected string field");
    };
    value
}

#[test]
fn init_state_and_handler_copy_out_work() {
    let plugin = load(PLUGIN, MANIFEST).expect("load");
    let doc = plugin.make_handle("Doc", "ok").unwrap();
    let mut instance = plugin.instantiate().expect("init");
    assert_eq!(state_int(&instance, 0), 0);
    assert_eq!(state_str(&instance, 1), "");

    let Outcome::Handled(value) = instance.call("saved", vec![doc]).expect("call") else {
        panic!("handler missing");
    };
    assert!(matches!(value, Value::Result(Ok(_))), "{value:?}");
    assert_eq!(state_int(&instance, 0), 1);
    assert_eq!(state_str(&instance, 1), "hello");
}

#[test]
fn handler_parameters_are_copy_in_and_copy_out() {
    let plugin = load(PLUGIN, MANIFEST).expect("load");
    let mut instance = plugin.instantiate().expect("init");
    let argument = Value::Int(1);
    let Outcome::Handled(result) = instance
        .call("mutate", vec![argument.clone()])
        .expect("call")
    else {
        panic!("handler missing");
    };
    assert_eq!(result, Value::Int(2));
    assert_eq!(argument, Value::Int(1), "caller value must not be mutated");
}

#[test]
fn a_trap_rolls_back_the_state_and_an_err_commits_it() {
    let plugin = load(PLUGIN, MANIFEST).expect("load");
    let mut instance = plugin.instantiate().expect("init");

    let error = instance.call("boom", vec![]).expect_err("trap");
    assert!(matches!(error, martite_runtime::Trap::Panic { .. }));
    assert_eq!(state_int(&instance, 0), 0, "trap must roll back");

    let Outcome::Handled(value) = instance.call("fail", vec![]).expect("call") else {
        panic!("handler missing");
    };
    assert!(matches!(value, Value::Result(Err(_))));
    assert_eq!(state_int(&instance, 0), 1, "Err commits the state");
}

#[test]
fn shutdown_and_stateless_plugins_work() {
    let plugin = load(PLUGIN, MANIFEST).expect("load");
    let mut instance = plugin.instantiate().expect("init");
    instance.call("tick", vec![]).expect("call");
    assert_eq!(state_int(&instance, 0), 1);
    instance.shutdown().expect("shutdown runs");

    let stateless = load("on check() {}\n", MANIFEST).expect("stateless plugin");
    let mut instance = stateless.instantiate().expect("instantiate");
    assert!(instance.state().is_none());
    assert!(matches!(
        instance.call("check", vec![]).expect("call"),
        Outcome::Handled(Value::Unit)
    ));
    assert!(matches!(
        instance.call("missing", vec![]).expect("missing hook"),
        Outcome::NoHandler
    ));
}

#[test]
fn settings_are_delivered_to_init_as_json() {
    let source = r#"
        use std.json
        use std.json.Json
        struct State { text: str }
        on init(settings: Json) -> State {
            State { text: json.stringify(settings) }
        }
        "#;
    let manifest = r#"
[plugin]
name = "settings"
version = "0.1.0"
lang = "1"
api = "1.0"
entry = "main.mt"

[settings]
max_words = 5000
enabled = true
"#;
    let plugin = {
        let mut provider = MemoryProvider::new();
        provider.insert("main.mt", source);
        Engine::new().load(manifest, &provider).expect("load")
    };
    let instance = plugin.instantiate().expect("init");
    let text = state_str(&instance, 0);
    assert!(text.contains("\"max_words\":5000.0"), "{text}");
    assert!(text.contains("\"enabled\":true"), "{text}");
}

#[test]
fn snapshots_round_trip_and_validate_their_type_fingerprint() {
    let plugin = load(PLUGIN, MANIFEST).expect("load");
    let doc = plugin.make_handle("Doc", "ok").unwrap();
    let mut instance = plugin.instantiate().expect("init");
    instance.call("saved", vec![doc]).expect("call");
    let snapshot = instance.snapshot();
    assert_ne!(snapshot.type_fingerprint, [0u8; 32]);
    assert!(!snapshot.bytes.is_empty());

    let mut restored = plugin.instantiate().expect("init");
    restored.restore(&snapshot).expect("restore");
    assert_eq!(state_int(&restored, 0), 1);
    assert_eq!(state_str(&restored, 1), "hello");

    let mut corrupt = snapshot.clone();
    corrupt.format_version = 99;
    assert!(restored.restore(&corrupt).is_err());
    let mut corrupt = snapshot.clone();
    corrupt.bytes.truncate(3);
    assert!(restored.restore(&corrupt).is_err());
    let mut wrong_type = snapshot.clone();
    wrong_type.type_fingerprint = [1u8; 32];
    assert!(restored.restore(&wrong_type).is_err());
}

#[test]
fn stale_handles_are_reported_as_errors_not_traps() {
    let plugin = load(PLUGIN, MANIFEST).expect("load");
    let stale = plugin.make_handle("Doc", "stale").unwrap();
    let mut instance = plugin.instantiate().expect("init");
    let Outcome::Handled(value) = instance.call("lookup", vec![stale]).expect("call") else {
        panic!("handler missing");
    };
    assert!(matches!(value, Value::Result(Err(_))), "{value:?}");
    assert_eq!(state_int(&instance, 0), 0);
}

#[test]
fn manifest_limits_are_clamped_by_the_engine_maximum() {
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", PLUGIN);
    let mut engine = editor_engine();
    engine.set_limits(
        Limits {
            fuel: 1_000_000,
            memory: 4,
            depth: 1,
            deadline: None,
        },
        Limits::default(),
    );
    let plugin = engine.load(MANIFEST, &provider).expect("load");
    // The manifest requests 1 MiB, but the host maximum clamps it to 4 bytes,
    // so `init` cannot even allocate its state.
    assert!(matches!(
        plugin.instantiate(),
        Err(martite_runtime::Trap::OutOfMemory)
    ));
}

#[test]
fn handler_state_and_arguments_count_toward_memory() {
    let interface = "on total(items: list[int]) -> int\n";
    let manifest = r#"
[plugin]
name = "memory"
version = "0.1.0"
lang = "1"
api = "1.0"
entry = "main.mt"

[permissions]
grant = ["app.api"]
"#;
    let source = r#"
use app.api
struct State { items: list[int] }
on init() -> State { State { items: [1, 2, 3] } }
on total(self, items: list[int]) -> int { self.items.len() + items.len() }
"#;
    let mut engine = Engine::new();
    engine.interface("app.api", interface);
    engine.set_limits(
        Limits {
            fuel: 1_000_000,
            memory: 80,
            depth: 100,
            deadline: None,
        },
        Limits::default(),
    );
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", source);
    let plugin = engine.load(manifest, &provider).expect("load");
    let mut instance = plugin.instantiate().expect("state fits");
    let argument = Value::list(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    assert!(matches!(
        instance.call("total", vec![argument]),
        Err(martite_runtime::Trap::OutOfMemory)
    ));
}

#[test]
fn reload_keeps_or_reinitializes_the_snapshot_by_fingerprint() {
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", PLUGIN);
    let engine = editor_engine();
    let plugin = engine.load(MANIFEST, &provider).expect("load");
    let doc = plugin.make_handle("Doc", "ok").unwrap();
    let mut instance = plugin.instantiate().expect("init");
    instance.call("saved", vec![doc]).expect("call");
    let snapshot = instance.snapshot();
    assert_eq!(state_int(&instance, 0), 1);

    let reloaded = engine
        .reload(MANIFEST, &provider, &plugin, &snapshot)
        .expect("reload");
    assert!(reloaded.snapshot_kept);
    let restored = reloaded.instantiate().expect("restore");
    assert_eq!(state_int(&restored, 0), 1);

    let changed = PLUGIN.replace(
        "struct State { saves: int, text: str }",
        "struct State { saves: int, text: str, extra: int }",
    );
    let changed = changed.replace(
        "State { saves: 0, text: \"\" }",
        "State { saves: 0, text: \"\", extra: 0 }",
    );
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", changed);
    let reloaded = engine
        .reload(MANIFEST, &provider, &plugin, &snapshot)
        .expect("reload");
    assert!(!reloaded.snapshot_kept);
    let reset = reloaded.instantiate().expect("init");
    assert_eq!(state_int(&reset, 0), 0);
}

#[test]
fn host_test_doubles_serve_offline_tests() {
    let interface = "fn answer() -> int\n";
    let manifest = r#"
[plugin]
name = "math-tests"
version = "0.1.0"
lang = "1"
api = "1.0"
entry = "main.mt"

[permissions]
grant = ["app.math"]
"#;
    let source = r#"
use app.math
use app.math.answer

test "test double answers" {
    assert(answer() == 42, "fake host function returned the wrong value")
}
"#;
    let mut engine = Engine::new();
    engine.interface("app.math", interface);
    engine.test_double_fn(
        "app.math.answer",
        HostSignature::new(vec![], HostType::Int),
        |_cx: &mut HostContext, _args| Ok(Value::Int(42)),
    );
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", source);
    let plugin = engine.load(manifest, &provider).expect("load");
    let reports = plugin.run_tests();
    assert_eq!(reports.len(), 1);
    assert!(reports[0].passed(), "{:?}", reports[0].outcome);
}

#[test]
fn ungranted_host_modules_fail_before_execution() {
    let manifest = MANIFEST.replace("app.editor", "app.other");
    let error = match load(PLUGIN, &manifest) {
        Ok(_) => panic!("expected a grant error"),
        Err(error) => error,
    };
    assert!(error.iter().any(|d| d.code == Code::E501));
}

/// Compile-time guard that the plugin API remains usable from the conformance
/// crate without exposing runtime internals.
#[test]
fn plugin_type_fingerprint_is_stable() {
    let plugin = load(PLUGIN, MANIFEST).expect("load");
    let first = plugin.type_fingerprint();
    let second = plugin.type_fingerprint();
    assert_eq!(first, second);
    assert_ne!(first, [0u8; 32]);
}
