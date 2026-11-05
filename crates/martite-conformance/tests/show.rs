use martite::{Engine, HostSignature, HostType, Instance, Outcome};
use martite_base::MemoryProvider;
use martite_conformance::run;

fn show_lines(body: &str) -> Vec<String> {
    run(&format!("fn main() {{\n{body}\n}}"))
}

#[test]
fn primitive_show_rows() {
    assert_eq!(
        show_lines(
            r#"
            print("{42}")
            print("{-7}")
            print("{2.0}")
            print("{0.1}")
            print("{1e21}")
            print("{-0.0}")
            print("{true}")
            print("{false}")
            let unit = ()
            print("{unit}")
            "#
        ),
        [
            "42", "-7", "2.0", "0.1", "1e21", "-0.0", "true", "false", "()"
        ]
    );
}

#[test]
fn float_edge_values() {
    assert_eq!(
        show_lines(
            r#"
            let nan = 0.0 / 0.0
            let inf = 1.0 / 0.0
            let neg_inf = -1.0 / 0.0
            print("{nan}")
            print("{inf}")
            print("{neg_inf}")
            print("{0.1 + 0.2}")
            "#
        ),
        ["NaN", "inf", "-inf", "0.30000000000000004"]
    );
}

#[test]
fn strings_are_raw_at_top_level_and_quoted_inside_containers() {
    let source = r#"
        fn main() {
            let text = "a\\b\"c\nd\te\rf"
            print("{text}")
            print("{[text]}")
            let a = "a"
            let b = "b"
            print("{(a, b)}")
            let table = {"key": "value"}
            print("{table}")
        }
    "#;
    assert_eq!(
        run(source),
        [
            "a\\b\"c\nd\te\rf",
            "[\"a\\\\b\\\"c\\nd\\te\\rf\"]",
            "(\"a\", \"b\")",
            "{\"key\": \"value\"}",
        ]
    );
}

#[test]
fn containers_ranges_structs_and_variants() {
    assert_eq!(
        run(r#"
            enum Color { Red, Green }
            enum Shape { Circle(float), Rect(float, float) }
            struct Point { x: int, y: int }
            fn main() {
                print("{[1, 2]}")
                let empty: list[int] = []
                print("{empty}")
                print("{(1,)}")
                let a = "a"
                let b = "b"
                print("{(a, b)}")
                print("{Color.Red}")
                print("{Shape.Circle(1.0)}")
                print("{Point { x: 1, y: 2 }}")
                print("{Some(1)}")
                let none: int? = None
                print("{none}")
                let ok: Result[(), str] = Ok(())
                let err: Result[(), str] = Err("x")
                print("{ok}")
                print("{err}")
                print("{0..3}")
                print("{0..=3}")
            }
            "#),
        [
            "[1, 2]",
            "[]",
            "(1,)",
            "(\"a\", \"b\")",
            "Color.Red",
            "Shape.Circle(1.0)",
            "Point { x: 1, y: 2 }",
            "Some(1)",
            "None",
            "Ok(())",
            "Err(\"x\")",
            "0..3",
            "0..=3",
        ]
    );
}

#[test]
fn map_show_uses_insertion_order_and_reinsert_moves_to_the_end() {
    assert_eq!(
        show_lines(
            r#"
            var table = {"b": 2, "a": 1}
            print("{table}")
            table["b"] = 3
            print("{table}")
            let _ = table.remove("b")
            print("{table}")
            table["b"] = 4
            print("{table}")
            "#
        ),
        [
            "{\"b\": 2, \"a\": 1}",
            "{\"b\": 3, \"a\": 1}",
            "{\"a\": 1}",
            "{\"a\": 1, \"b\": 4}",
        ]
    );
}

#[test]
fn opaque_handles_print_the_type_name() {
    let interface = "struct Doc\nfn doc() -> Doc\non describe(doc: Doc) -> str\n";
    let manifest = r#"
[plugin]
name = "show-handle"
version = "0.1.0"
lang = "1"
api = "1.0"
entry = "main.mt"

[permissions]
grant = ["app.api"]
"#;
    let source = "use app.api.Doc\non describe(doc: Doc) -> str { str(doc) }\n";
    let mut engine = Engine::new();
    engine.interface("app.api", interface);
    // The host function is declared by the interface but never called by this
    // fixture; registration only satisfies load-time E509 checking.
    engine.host_fn(
        "app.api.doc",
        HostSignature::new(vec![], HostType::Handle("Doc".into())),
        |_cx, _args| Ok(martite::HostValue::Unit),
    );
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", source);
    let plugin = engine.load(manifest, &provider).expect("plugin loads");
    let mut instance: Instance<'_> = plugin.instantiate().expect("instantiate");
    let handle = plugin.make_handle("Doc", "doc-1").expect("handle");
    let Outcome::Handled(value) = instance.call("describe", vec![handle]).expect("hook") else {
        panic!("handler missing");
    };
    assert_eq!(value.as_str(), Some("<Doc>"));
}
