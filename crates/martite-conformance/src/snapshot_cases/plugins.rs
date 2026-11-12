use martite::{HostSignature, HostType};
use martite_runtime::{HostFault, Value};

use crate::snapshot::{ArgSpec, HostSet, PluginStep, SnapshotCase};
use crate::snapshot_cases::{default_caps, plugin_case};

const INTERFACE: &str = r#"struct Doc
fn text(doc: Doc) -> str
fn new_doc(name: str) -> Doc
fn maybe() -> str?
fn stale_text(doc: Doc) -> str?
on saved(doc: Doc) -> Result[(), str]
on lookup(doc: Doc) -> Result[(), str]
on create() -> Result[(), str]
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

on create(mut self) -> Result[(), str] {
    let doc = editor.new_doc("ok")
    self.text = editor.text(doc)
    self.saves += 1
    Ok(())
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

fn editor_hosts() -> HostSet {
    let mut hosts = HostSet::new();
    hosts.add(
        "app.editor.text",
        HostSignature::new(vec![HostType::Handle("Doc".into())], HostType::Str),
        |_cx, args| match args.first() {
            Some(Value::Handle(handle)) if handle.token == "ok" => Ok(Value::str("hello")),
            Some(Value::Handle(_)) => Err(HostFault::new("stale handle")),
            _ => Err(HostFault::new("expected a Doc handle")),
        },
    );
    hosts.add(
        "app.editor.new_doc",
        HostSignature::new(vec![HostType::Str], HostType::Handle("Doc".into())),
        |cx, args| {
            let name = args.first().and_then(Value::as_str).unwrap_or_default();
            cx.make_handle(name)
        },
    );
    hosts.add(
        "app.editor.maybe",
        HostSignature::new(vec![], HostType::Option(Box::new(HostType::Str))),
        |_cx, _args| Ok(Value::Option(Some(Box::new(Value::str("maybe"))))),
    );
    hosts.add(
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
    hosts
}

fn files(source: &str) -> Vec<(String, String)> {
    vec![
        ("main.mt".into(), source.into()),
        ("app/editor.mti".into(), INTERFACE.into()),
    ]
}

fn scenario(
    id: &'static str,
    source: &str,
    hosts: Option<HostSet>,
    steps: Vec<PluginStep>,
) -> SnapshotCase {
    plugin_case(
        format!("plugins/{id}"),
        MANIFEST,
        files(source),
        hosts,
        steps,
        default_caps(),
    )
}

pub fn cases() -> Vec<SnapshotCase> {
    let mut cases = vec![
        scenario(
            "init_state_and_handler_copy_out_work",
            PLUGIN,
            Some(editor_hosts()),
            vec![
                PluginStep::Init,
                PluginStep::Call {
                    hook: "saved",
                    args: vec![ArgSpec::handle("Doc", "ok")],
                },
            ],
        ),
        scenario(
            "host_functions_can_create_handles_during_a_call",
            PLUGIN,
            Some(editor_hosts()),
            vec![
                PluginStep::Init,
                PluginStep::Call {
                    hook: "create",
                    args: Vec::new(),
                },
            ],
        ),
        scenario(
            "handler_parameters_are_copy_in_and_copy_out",
            PLUGIN,
            Some(editor_hosts()),
            vec![
                PluginStep::Init,
                PluginStep::Call {
                    hook: "mutate",
                    args: vec![ArgSpec::int(1)],
                },
            ],
        ),
        scenario(
            "a_trap_rolls_back_the_state_and_an_err_commits_it",
            PLUGIN,
            Some(editor_hosts()),
            vec![
                PluginStep::Init,
                PluginStep::Call {
                    hook: "boom",
                    args: Vec::new(),
                },
                PluginStep::Call {
                    hook: "fail",
                    args: Vec::new(),
                },
            ],
        ),
        scenario(
            "shutdown_and_stateless_plugins_work",
            PLUGIN,
            Some(editor_hosts()),
            vec![
                PluginStep::Init,
                PluginStep::Call {
                    hook: "tick",
                    args: Vec::new(),
                },
                PluginStep::Shutdown,
            ],
        ),
        scenario(
            "stateless_plugin_handlers",
            "on check() {}\n",
            Some(editor_hosts()),
            vec![
                PluginStep::Call {
                    hook: "check",
                    args: Vec::new(),
                },
                PluginStep::Call {
                    hook: "missing",
                    args: Vec::new(),
                },
            ],
        ),
        scenario(
            "stale_handles_are_reported_as_errors_not_traps",
            PLUGIN,
            Some(editor_hosts()),
            vec![
                PluginStep::Init,
                PluginStep::Call {
                    hook: "lookup",
                    args: vec![ArgSpec::handle("Doc", "stale")],
                },
            ],
        ),
    ];
    cases.push(plugin_case(
        "plugins/settings_are_delivered_to_init_as_json",
        r#"
[plugin]
name = "settings"
version = "0.1.0"
lang = "1"
api = "1.0"
entry = "main.mt"

[settings]
max_words = 5000
enabled = true
"#,
        vec![(
            "main.mt".into(),
            r#"
        use std.json
        use std.json.Json
        struct State { text: str }
        on init(settings: Json) -> State {
            State { text: json.stringify(settings) }
        }
        "#
            .into(),
        )],
        None,
        vec![PluginStep::Init],
        default_caps(),
    ));
    cases
}
