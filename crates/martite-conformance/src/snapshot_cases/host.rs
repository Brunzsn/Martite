use martite::{HostSignature, HostType};
use martite_runtime::Value;

use crate::snapshot::{ArgSpec, HostSet, PluginStep, SnapshotCase};
use crate::snapshot_cases::{default_caps, plugin_case};

const INTERFACE: &str = r#"struct Doc
fn text(doc: Doc) -> str
fn new_doc(name: str) -> Doc
fn maybe() -> str?
fn stale_text(doc: Doc) -> str?
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

const HANDLE_INTERFACE: &str = r#"struct Doc
fn new_doc(name: str) -> Doc
fn find_doc(name: str) -> Doc?
fn try_new_doc(name: str) -> Result[Doc, str]
fn text(doc: Doc) -> str
fn bad() -> str
on created() -> Result[(), str]
on found() -> Result[(), str]
on try_new() -> Result[(), str]
on broken() -> Result[(), str]
"#;

const HANDLE_MANIFEST: &str = r#"
[plugin]
name = "handle-constructor"
version = "0.1.0"
lang = "1"
api = "1.0"
entry = "main.mt"

[permissions]
grant = ["app.docs"]
"#;

const HANDLE_PLUGIN: &str = r#"
use app.docs

struct State { text: str, count: int }

on init() -> State { State { text: "", count: 0 } }

on created(mut self) -> Result[(), str] {
    let doc = docs.new_doc("new")
    self.text = docs.text(doc)
    self.count += 1
    Ok(())
}

on found(mut self) -> Result[(), str] {
    match docs.find_doc("found") {
        Some(doc) => {
            self.text = docs.text(doc)
            self.count += 1
            Ok(())
        }
        None => Err("missing"),
    }
}

on try_new(mut self) -> Result[(), str] {
    match docs.try_new_doc("tried") {
        Ok(doc) => {
            self.text = docs.text(doc)
            self.count += 1
            Ok(())
        }
        Err(message) => Err(message),
    }
}

on broken() -> Result[(), str] {
    let _ = docs.bad()
    Ok(())
}
"#;

fn editor_hosts() -> HostSet {
    let mut hosts = HostSet::new();
    hosts.add(
        "app.editor.text",
        HostSignature::new(vec![HostType::Handle("Doc".into())], HostType::Str),
        |_cx, _args| Ok(Value::str("hello world")),
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
        |_cx, _args| Ok(Value::Option(None)),
    );
    hosts
}

fn handle_hosts() -> HostSet {
    let mut hosts = HostSet::new();
    hosts.add(
        "app.docs.new_doc",
        HostSignature::new(vec![HostType::Str], HostType::Handle("Doc".into())),
        |cx, args| {
            let name = args.first().and_then(Value::as_str).unwrap_or_default();
            cx.make_handle(name)
        },
    );
    hosts.add(
        "app.docs.find_doc",
        HostSignature::new(
            vec![HostType::Str],
            HostType::Option(Box::new(HostType::Handle("Doc".into()))),
        ),
        |cx, args| {
            let name = args.first().and_then(Value::as_str).unwrap_or_default();
            Ok(Value::Option(Some(Box::new(cx.make_handle(name)?))))
        },
    );
    hosts.add(
        "app.docs.try_new_doc",
        HostSignature::new(
            vec![HostType::Str],
            HostType::Result(
                Box::new(HostType::Handle("Doc".into())),
                Box::new(HostType::Str),
            ),
        ),
        |cx, args| {
            let name = args.first().and_then(Value::as_str).unwrap_or_default();
            Ok(Value::Result(Ok(Box::new(cx.make_handle(name)?))))
        },
    );
    hosts.add(
        "app.docs.text",
        HostSignature::new(vec![HostType::Handle("Doc".into())], HostType::Str),
        |_cx, args| match args.first() {
            Some(Value::Handle(handle)) => Ok(Value::str(handle.token.clone())),
            _ => Ok(Value::str("")),
        },
    );
    hosts.add(
        "app.docs.bad",
        HostSignature::new(vec![], HostType::Str),
        |cx, _args| cx.make_handle("bad"),
    );
    hosts
}

fn editor_scenario(id: &'static str, steps: Vec<PluginStep>) -> SnapshotCase {
    plugin_case(
        format!("host/{id}"),
        MANIFEST,
        vec![
            ("main.mt".into(), PLUGIN.into()),
            ("app/editor.mti".into(), INTERFACE.into()),
        ],
        Some(editor_hosts()),
        steps,
        default_caps(),
    )
}

fn handle_scenario(id: &'static str, hook: &'static str) -> SnapshotCase {
    plugin_case(
        format!("host/{id}"),
        HANDLE_MANIFEST,
        vec![
            ("main.mt".into(), HANDLE_PLUGIN.into()),
            ("app/docs.mti".into(), HANDLE_INTERFACE.into()),
        ],
        Some(handle_hosts()),
        vec![
            PluginStep::Init,
            PluginStep::Call {
                hook,
                args: Vec::new(),
            },
        ],
        default_caps(),
    )
}

pub fn cases() -> Vec<SnapshotCase> {
    vec![
        editor_scenario(
            "plugin_init_hook_and_state",
            vec![
                PluginStep::Init,
                PluginStep::Call {
                    hook: "saved",
                    args: vec![ArgSpec::handle("Doc", "doc-1")],
                },
            ],
        ),
        handle_scenario("host_functions_can_construct_handles", "created"),
        handle_scenario("host_functions_can_construct_option_handles", "found"),
        handle_scenario("host_functions_can_construct_result_handles", "try_new"),
        handle_scenario(
            "make_handle_without_an_opaque_return_is_a_host_fault",
            "broken",
        ),
    ]
}
