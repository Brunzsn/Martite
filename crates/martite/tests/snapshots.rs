use martite::{Engine, Instance, Plugin, Snapshot};
use martite_base::MemoryProvider;
use martite_runtime::Value;

const STATE_INTERFACE: &str = "on bump()\n";

const STATE_MANIFEST: &str = r#"
[plugin]
name = "snapshot-state"
version = "0.1.0"
lang = "1"
api = "1.0"
entry = "main.mt"

[permissions]
grant = ["app.state"]
"#;

const STATE_PLUGIN: &str = r#"
use app.state

struct State { count: int }

on init() -> State { State { count: 0 } }
on bump(mut self) { self.count += 1 }
"#;

const STATELESS_PLUGIN: &str = r#"
use app.state

on bump() {}
"#;

const DAG_MANIFEST: &str = r#"
[plugin]
name = "snapshot-dag"
version = "0.1.0"
lang = "1"
api = "1.0"
entry = "main.mt"

[limits]
fuel = 1000000
memory = 1048576
depth = 200
"#;

const DAG_PLUGIN: &str = r#"
enum Tree { Leaf, Node(Tree, Tree) }

fn build(n: int) -> Tree {
    var v = Tree.Leaf
    var i = 0
    while i < n {
        v = Tree.Node(v, v)
        i += 1
    }
    v
}

on init() -> Tree { build(40) }
"#;

const DOC_INTERFACE: &str = "struct Doc\non remember(doc: Doc)\n";

const DOC_MANIFEST: &str = r#"
[plugin]
name = "snapshot-handles"
version = "0.1.0"
lang = "1"
api = "1.0"
entry = "main.mt"

[permissions]
grant = ["app.editor"]
"#;

const DOC_PLUGIN: &str = r#"
use app.editor
use app.editor.Doc

struct State { doc: Doc? }

on init() -> State { State { doc: None } }
on remember(mut self, doc: Doc) { self.doc = Some(doc) }
"#;

fn load_with(engine: &Engine, manifest: &str, source: &str) -> Plugin {
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", source);
    engine
        .try_load(manifest, &provider)
        .unwrap_or_else(|error| {
            panic!(
                "{}",
                error
                    .diagnostics
                    .iter()
                    .map(|d| d.render(&error.sources))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        })
}

fn load(manifest: &str, source: &str) -> Plugin {
    load_with(&Engine::new(), manifest, source)
}

fn state_engine() -> Engine {
    let mut engine = Engine::new();
    engine.interface("app.state", STATE_INTERFACE);
    engine
}

fn state_count(instance: &Instance) -> i64 {
    let Some(Value::Struct(state)) = instance.state() else {
        panic!("expected struct state");
    };
    let Value::Int(count) = &state.fields[0] else {
        panic!("expected count field");
    };
    *count
}

#[test]
fn shared_dag_state_errors_instead_of_expanding_exponentially() {
    let plugin = load(DAG_MANIFEST, DAG_PLUGIN);
    let instance = plugin.instantiate().expect("init");

    let error = instance
        .snapshot()
        .expect_err("a 40-level shared DAG must exceed the default snapshot budget");
    assert!(error.contains("budget"), "{error}");

    let error = instance
        .snapshot_with_limit(1024)
        .expect_err("a small host budget must reject the expanded DAG");
    assert!(error.contains("budget"), "{error}");
}

#[test]
fn empty_payload_is_only_valid_for_a_stateless_plugin() {
    let plugin = load_with(&state_engine(), STATE_MANIFEST, STATE_PLUGIN);
    let snapshot = plugin
        .instantiate()
        .expect("init")
        .snapshot()
        .expect("snapshot");

    let empty = Snapshot {
        format_version: snapshot.format_version,
        type_fingerprint: snapshot.type_fingerprint,
        bytes: Vec::new(),
    };
    let mut instance = plugin.instantiate().expect("init");
    let error = instance
        .restore(&empty)
        .expect_err("an empty payload must not reset a stateful plugin");
    assert!(error.contains("empty"), "{error}");
    assert_eq!(state_count(&instance), 0);

    let stateless = load_with(&state_engine(), STATE_MANIFEST, STATELESS_PLUGIN);
    let snapshot = stateless
        .instantiate()
        .expect("instantiate")
        .snapshot()
        .expect("snapshot");
    let mut restored = stateless.instantiate().expect("instantiate");
    restored.restore(&snapshot).expect("stateless restore");
    assert!(restored.state().is_none());

    let mut forged = snapshot.clone();
    forged.bytes = vec![1, 0, 0, 0, 0, 0, 0, 0, 0];
    assert!(restored.restore(&forged).is_err());
}

#[test]
fn restore_rejects_snapshots_larger_than_the_budget() {
    let plugin = load_with(&state_engine(), STATE_MANIFEST, STATE_PLUGIN);
    let instance = plugin.instantiate().expect("init");
    let snapshot = instance.snapshot().expect("snapshot");
    assert!(!snapshot.bytes.is_empty());

    let mut restored = plugin.instantiate().expect("init");
    let error = restored
        .restore_with_limit(&snapshot, 1)
        .expect_err("the byte cap must be checked before decoding");
    assert!(error.contains("budget"), "{error}");
}

#[test]
fn reload_reinitializes_when_the_snapshot_format_version_changes() {
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", STATE_PLUGIN);
    let engine = state_engine();
    let plugin = engine.load(STATE_MANIFEST, &provider).expect("load");
    let mut instance = plugin.instantiate().expect("init");
    instance.call("bump", vec![]).expect("bump");
    assert_eq!(state_count(&instance), 1);

    let mut snapshot = instance.snapshot().expect("snapshot");
    snapshot.format_version = 99;
    let reloaded = engine
        .reload(STATE_MANIFEST, &provider, &plugin, &snapshot)
        .expect("reload");
    assert!(
        !reloaded.snapshot_kept,
        "a version mismatch must not keep the snapshot"
    );
    let reset = reloaded.instantiate().expect("init");
    assert_eq!(
        state_count(&reset),
        0,
        "init must run after a version change"
    );
}

#[test]
fn handle_restore_requires_the_host_validator_and_rejects_replays() {
    let mut engine = Engine::new();
    engine.interface("app.editor", DOC_INTERFACE);
    engine.set_snapshot_handle_validator(|ty, token| ty == "Doc" && token == "session-a");
    let plugin = load_with(&engine, DOC_MANIFEST, DOC_PLUGIN);

    let doc = plugin.make_handle("Doc", "session-a").expect("handle");
    let mut instance = plugin.instantiate().expect("init");
    instance.call("remember", vec![doc]).expect("remember");
    let snapshot = instance.snapshot().expect("snapshot");

    let mut restored = plugin.instantiate().expect("init");
    restored.restore(&snapshot).expect("current session handle");
    assert!(matches!(restored.state(), Some(Value::Struct(_))));

    let replay = plugin.make_handle("Doc", "session-b").expect("handle");
    let mut other = plugin.instantiate().expect("init");
    other.call("remember", vec![replay]).expect("remember");
    let replayed = other.snapshot().expect("snapshot");
    let error = restored.restore(&replayed).expect_err("replayed handle");
    assert!(error.contains("rejected"), "{error}");
    assert!(
        matches!(restored.state(), Some(Value::Struct(_))),
        "a failed restore must leave the state unchanged"
    );

    let mut plain = Engine::new();
    plain.interface("app.editor", DOC_INTERFACE);
    let plain_plugin = load_with(&plain, DOC_MANIFEST, DOC_PLUGIN);
    let mut plain_instance = plain_plugin.instantiate().expect("init");
    let error = plain_instance
        .restore(&snapshot)
        .expect_err("handles are rejected without a validator");
    assert!(error.contains("validator"), "{error}");
}
