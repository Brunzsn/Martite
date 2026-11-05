use martite::{Engine, HostSignature, HostType};
use martite_base::{Code, Diagnostic, MemoryProvider};
use martite_conformance::{Case, assert_clean, assert_diagnostic};

const EDITOR: &str = r#"struct Doc
fn text(doc: Doc) -> str
on saved(doc: Doc) -> Result[(), str]
on check()
on a()
on b()
"#;

fn editor_case(source: &str) -> Case {
    Case::single(source)
        .interface("app.editor", EDITOR)
        .grant("app.editor")
}

fn assert_engine_code(result: Result<martite::Plugin, Vec<Diagnostic>>, code: Code, message: &str) {
    match result {
        Ok(_) => panic!("expected {code}, got a successful load"),
        Err(diagnostics) => {
            assert!(
                diagnostics.iter().any(|d| d.code == code
                    && d.message.to_lowercase().contains(&message.to_lowercase())),
                "expected {code} containing {message:?}, got {diagnostics:#?}"
            );
        }
    }
}

#[test]
fn plugin_load_errors_have_pass_and_fail_fixtures() {
    // E501: use of an ungranted host module.
    let fail = Case::single("use app.editor\nfn main() {}").interface("app.editor", EDITOR);
    assert_diagnostic(&fail, Code::E501, None, Some("not granted"), None);
    assert_clean(&editor_case("use app.editor\nfn main() {}"));

    // E502: handler with no matching hook.
    assert_diagnostic(
        &editor_case("on nope() {}\n"),
        Code::E502,
        None,
        Some("hook"),
        None,
    );
    assert_clean(&editor_case("on check() {}\n"));

    // E503: handler signature differs from the hook.
    assert_diagnostic(
        &editor_case("on saved() -> Result[(), str] { Ok(()) }\n"),
        Code::E503,
        None,
        Some("signature"),
        None,
    );
    assert_clean(&editor_case(
        "use app.editor\nuse app.editor.Doc\non saved(doc: Doc) -> Result[(), str] { Ok(()) }\n",
    ));

    // E504: `mut self` without an `init`.
    assert_diagnostic(
        &editor_case("on saved(mut self, doc: Doc) -> Result[(), str] { Ok(()) }\n"),
        Code::E504,
        None,
        Some("init"),
        None,
    );
    assert_clean(&editor_case(
        "use app.editor.Doc\nstruct State { n: int }\non init() -> State { State { n: 0 } }\non saved(mut self, doc: Doc) -> Result[(), str] { Ok(()) }\n",
    ));

    // E505: handler called outside a test block.
    assert_diagnostic(
        &editor_case("on a() { b() }\non b() {}\n"),
        Code::E505,
        None,
        Some("test"),
        None,
    );
    assert_clean(&editor_case(
        "on a() {}\non b() {}\ntest \"calls\" { a() }\n",
    ));

    // E506: function type in the state type.
    assert_diagnostic(
        &editor_case("on init() -> fn() -> int { fn() => 1 }\n"),
        Code::E506,
        None,
        Some("function"),
        None,
    );
    assert_clean(&editor_case("on init() -> int { 1 }\n"));

    // E507: reserved hook name in an interface.
    let fail = Case::single("fn main() {}").interface("bad.iface", "on init() -> int\n");
    assert_diagnostic(&fail, Code::E507, None, Some("reserved"), None);
    let fail = Case::single("fn main() {}").interface("bad.iface", "on tick()\non tick()\n");
    assert_diagnostic(&fail, Code::E507, None, Some("already declared"), None);
    let fail = Case::single("fn main() {}").interface("bad.iface", "struct Doc[T]\n");
    assert_diagnostic(&fail, Code::E507, None, Some("type parameter"), None);
    let pass = Case::single("fn main() {}").interface("ok.iface", "on tick()\n");
    assert_clean(&pass);
}

fn base_manifest() -> String {
    r#"
[plugin]
name = "word-count"
version = "0.1.0"
lang = "1"
api = "1.0"
entry = "main.mt"

[permissions]
grant = ["app.editor"]
"#
    .to_string()
}

fn editor_engine() -> Engine {
    let mut engine = Engine::new();
    engine.interface("app.editor", EDITOR);
    engine.host_fn(
        "app.editor.text",
        HostSignature::new(vec![HostType::Handle("Doc".into())], HostType::Str),
        |_cx, _args| Ok(martite::HostValue::str("text")),
    );
    engine
}

fn plugin_provider() -> MemoryProvider {
    let mut provider = MemoryProvider::new();
    provider.insert(
        "main.mt",
        "use app.editor\nuse app.editor.Doc\nstruct State { n: int }\non init() -> State { State { n: 0 } }\non saved(mut self, doc: Doc) -> Result[(), str] { Ok(()) }\n",
    );
    provider
}

#[test]
fn manifest_errors_are_e508() {
    let engine = Engine::new();
    let provider = MemoryProvider::new();

    assert_engine_code(
        engine.load("this is not toml", &provider),
        Code::E508,
        "malformed",
    );
    assert_engine_code(
        engine.load(
            &base_manifest().replace("entry = \"main.mt\"", "entry = \"main.mt\"\nnope = true"),
            &provider,
        ),
        Code::E508,
        "unknown",
    );
    assert_engine_code(
        engine.load(
            &base_manifest().replace("lang = \"1\"", "lang = \"2\""),
            &provider,
        ),
        Code::E508,
        "language",
    );
    assert_engine_code(
        engine.load(
            &base_manifest().replace("api = \"1.0\"", "api = \"2.0\""),
            &provider,
        ),
        Code::E508,
        "API",
    );
    assert_engine_code(
        engine.load(
            &base_manifest().replace("api = \"1.0\"", "api = \"1.1\""),
            &provider,
        ),
        Code::E508,
        "minor",
    );
    assert_engine_code(
        engine.load(
            &base_manifest().replace("entry = \"main.mt\"", "entry = \"missing.mt\""),
            &MemoryProvider::new(),
        ),
        Code::E508,
        "entry",
    );
    let engine = editor_engine();
    assert_engine_code(
        engine.load(
            &base_manifest().replace("app.editor", "app.unknown"),
            &plugin_provider(),
        ),
        Code::E508,
        "does not exist",
    );

    // Passing manifest and plugin.
    let engine = editor_engine();
    let plugin = engine
        .load(&base_manifest(), &plugin_provider())
        .expect("valid plugin");
    assert!(plugin.type_fingerprint() != [0u8; 32]);
}

#[test]
fn host_registration_errors_are_e509() {
    let manifest = base_manifest();
    let provider = plugin_provider();

    // Missing Rust implementation for a host function.
    let engine = {
        let mut engine = Engine::new();
        engine.interface("app.editor", EDITOR);
        engine
    };
    assert_engine_code(
        engine.load(&manifest, &provider),
        Code::E509,
        "no Rust implementation",
    );

    // Registered signature does not match the interface.
    let mut engine = Engine::new();
    engine.interface("app.editor", EDITOR);
    engine.host_fn(
        "app.editor.text",
        HostSignature::new(vec![HostType::Str], HostType::Str),
        |_cx, _args| Ok(martite::HostValue::str("text")),
    );
    assert_engine_code(engine.load(&manifest, &provider), Code::E509, "mismatched");

    // Host functions may not declare `mut` parameters.
    let mut engine = Engine::new();
    engine.interface(
        "app.editor",
        EDITOR.replace("fn text(doc: Doc)", "fn text(mut doc: Doc)"),
    );
    engine.host_fn(
        "app.editor.text",
        HostSignature::new(vec![HostType::Handle("Doc".into())], HostType::Str),
        |_cx, _args| Ok(martite::HostValue::str("text")),
    );
    assert_engine_code(engine.load(&manifest, &provider), Code::E509, "mut");

    // A correctly registered host function loads.
    let engine = editor_engine();
    assert!(engine.load(&manifest, &provider).is_ok());
}
