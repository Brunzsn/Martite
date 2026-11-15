//! Exhaustive builtin surface tests.
//!
//! The canonical tables in `martite-builtin` are the single source of truth for
//! builtin names, arities, signatures, effects and safepoints. These tests walk
//! every row through the full pipeline: resolve, check, lower, verify and run.

use martite_base::Code;
use martite_conformance::{Case, codes, run, trap};
use martite_ir::{BUILTIN_FNS, BUILTIN_ITEMS, BUILTIN_METHODS, BuiltinReceiver};

fn assert_mentions(source: &str, needle: &str, context: &str) {
    assert!(
        source.contains(needle),
        "{context}: fixture does not mention `{needle}`"
    );
}

#[test]
fn every_builtin_item_is_exercised_directly_and_as_a_value() {
    let direct_math = r#"
use std.math
fn main() {
    print("{math.PI}")
    print("{math.EULER}")
    print("{math.min(1, 2)}")
    print("{math.max(1, 2)}")
    print("{math.clamp(5, 0, 3)}")
    print("{math.sin(0.0)}")
    print("{math.cos(0.0)}")
    print("{math.tan(0.0)}")
    print("{math.atan2(1.0, 2.0)}")
    print("{math.exp(0.0)}")
    print("{math.ln(1.0)}")
    print("{math.log10(1.0)}")
}
"#;
    run(direct_math);

    let values_math = r#"
use std.math
fn main() {
    let pi = math.PI
    let euler = math.EULER
    let min = math.min
    let max = math.max
    let clamp = math.clamp
    let sin = math.sin
    let cos = math.cos
    let tan = math.tan
    let atan2 = math.atan2
    let exp = math.exp
    let ln = math.ln
    let log10 = math.log10
    print("{pi + euler}")
    print("{sin(0.0) + cos(0.0) + tan(0.0) + atan2(1.0, 2.0)}")
    print("{exp(0.0) + ln(1.0) + log10(1.0)}")
    print("{min(1, 2) + max(1, 2) + clamp(5, 0, 3)}")
}
"#;
    run(values_math);

    let direct_json = r#"
use std.json
use std.json.Json
fn main() {
    let parsed = json.parse("null")
    let value: Json = parsed.expect("json")
    print("{json.stringify(value)}")
}
"#;
    run(direct_json);

    let values_json = r#"
use std.json
use std.json.Json
fn main() {
    let parse = json.parse
    let stringify = json.stringify
    let value: Json = parse("null").expect("json")
    print("{stringify(value)}")
}
"#;
    run(values_json);

    for item in BUILTIN_ITEMS {
        let prefix = if item.module == "std.math" {
            "math."
        } else {
            "json."
        };
        let needle = format!("{prefix}{}", item.name);
        let source = if item.module == "std.math" {
            format!("{direct_math}\n{values_math}")
        } else {
            format!("{direct_json}\n{values_json}")
        };
        assert_mentions(&source, &needle, "builtin item");
    }
}

#[test]
fn every_builtin_function_is_exercised_directly_and_as_a_value() {
    let source = r#"
fn main() {
    print("direct")
    assert(true, "direct")
    let p = print
    let _ = p
    let a = assert
    a(true, "value")
}
"#;
    run(source);

    trap("fn main() { panic(\"direct\") }");
    trap("fn main() { let p = panic\np(\"value\") }");

    let coverage = format!(
        "{source}\nfn fail() {{ panic(\"direct\") }}\nfn fail_value() {{ let p = panic\np(\"value\") }}\n"
    );
    for function in BUILTIN_FNS {
        assert_mentions(&coverage, function.name, "builtin function");
    }
}

fn assert_methods_covered(receiver: BuiltinReceiver, source: &str) {
    for method in BUILTIN_METHODS
        .iter()
        .filter(|method| method.receiver == receiver)
    {
        assert_mentions(
            source,
            &format!(".{}(", method.name),
            &format!("{} method", receiver.as_str()),
        );
    }
}

#[test]
fn every_builtin_method_is_exercised() {
    let list = r#"
fn main() {
    var xs = [3, 1, 2]
    print("{xs.len()}")
    print("{xs.get(0)}")
    print("{xs.first()}")
    print("{xs.last()}")
    print("{xs.contains(2)}")
    print("{xs.map(fn(x) => x + 1)}")
    print("{xs.filter(fn(x) => x > 1)}")
    print("{xs.fold(0, fn(acc, x) => acc + x)}")
    print("{xs.any(fn(x) => x > 2)}")
    print("{xs.all(fn(x) => x > 0)}")
    print("{xs.enumerate()}")
    print("{xs.reversed()}")
    print("{xs.sorted()}")
    print("{xs.sorted_by(fn(x) => x)}")
    let words = ["a", "b"]
    let joined = words.join(",")
    print("{joined}")
    xs.push(4)
    print("{xs.pop()}")
    xs.sort()
    xs.insert(0, 9)
    print("{xs.remove_at(0)}")
    xs.extend([7, 8])
    xs.clear()
}
"#;
    run(list);
    assert_methods_covered(BuiltinReceiver::List, list);

    let map = r#"
fn main() {
    var m = {"a": 1}
    print("{m.len()}")
    let got = m.get("a")
    print("{got}")
    let has = m.has("b")
    print("{has}")
    print("{m.keys()}")
    print("{m.values()}")
    print("{m.entries()}")
    let removed = m.remove("a")
    print("{removed}")
    m.clear()
}
"#;
    run(map);
    assert_methods_covered(BuiltinReceiver::Map, map);

    let text = r#"
fn main() {
    let s = " Hello,42 "
    print("{s.len()}")
    print("{s.chars()}")
    let parts = s.split(",")
    print("{parts}")
    print("{s.trim()}")
    let contains = s.contains("Hello")
    print("{contains}")
    let find = s.find("42")
    print("{find}")
    let starts = s.starts_with(" ")
    print("{starts}")
    let ends = s.ends_with(" ")
    print("{ends}")
    let replaced = s.replace("Hello", "Bye")
    print("{replaced}")
    print("{s.to_upper()}")
    print("{s.to_lower()}")
    let parsed_int = "42".parse_int()
    print("{parsed_int}")
    let parsed_float = "4.5".parse_float()
    print("{parsed_float}")
}
"#;
    run(text);
    assert_methods_covered(BuiltinReceiver::Str, text);

    let numerics = r#"
fn main() {
    let n = -3
    print("{n.abs()}")
    print("{2.pow(3)}")
    let f = -2.5
    print("{f.abs()}")
    print("{f.floor()}")
    print("{f.ceil()}")
    print("{f.trunc()}")
    print("{4.0.sqrt()}")
    print("{f.pow(2.0)}")
    print("{f.round(0)}")
    print("{f.is_nan()}")
    print("{f.is_finite()}")
    let r = 0..3
    print("{r.len()}")
    print("{r.contains(1)}")
    print("{r.to_list()}")
}
"#;
    run(numerics);
    assert_methods_covered(BuiltinReceiver::Int, numerics);
    assert_methods_covered(BuiltinReceiver::Float, numerics);
    assert_methods_covered(BuiltinReceiver::Range, numerics);

    let option = r#"
fn main() {
    let some: int? = Some(2)
    print("{some.is_some()}")
    print("{some.is_none()}")
    print("{some.map(fn(x) => x + 1)}")
    print("{some.and_then(fn(x) => Some(x + 1))}")
    print("{some.unwrap_or(0)}")
    let result = some.ok_or("no")
    print("{result}")
    let unwrapped = some.expect("some")
    print("{unwrapped}")
}
"#;
    run(option);
    assert_methods_covered(BuiltinReceiver::Option, option);

    let result = r#"
fn main() {
    let ok: Result[int, str] = Ok(2)
    print("{ok.is_ok()}")
    print("{ok.is_err()}")
    print("{ok.map(fn(x) => x + 1)}")
    let mapped = ok.map_err(fn(message) => message + "!")
    print("{mapped}")
    print("{ok.and_then(fn(x) => Ok(x + 1))}")
    print("{ok.unwrap_or(0)}")
    print("{ok.ok()}")
    let unwrapped = ok.expect("ok")
    print("{unwrapped}")
}
"#;
    run(result);
    assert_methods_covered(BuiltinReceiver::Result, result);
}

#[test]
fn atan2_and_clamp_stay_correct_in_both_forms() {
    let logs = run(r#"
use std.math
fn main() {
    let angle: float = math.atan2(1.0, 2.0)
    let clamped = math.clamp(5, 0, 3)
    let f = math.clamp
    let g = math.atan2
    print("{angle}")
    print("{clamped}")
    print("{f(9, 0, 7)}")
    print("{g(1.0, 2.0)}")
}
"#);
    assert_eq!(logs, ["0.4636476090008061", "3", "7", "0.4636476090008061"]);
}

fn receiver_declaration(receiver: BuiltinReceiver) -> &'static str {
    match receiver {
        BuiltinReceiver::List => "var receiver = [1]\n",
        BuiltinReceiver::Map => "var receiver = {\"k\": 1}\n",
        BuiltinReceiver::Str => "var receiver = \"x\"\n",
        BuiltinReceiver::Int => "var receiver = 1\n",
        BuiltinReceiver::Float => "var receiver = 1.0\n",
        BuiltinReceiver::Range => "var receiver = 0..1\n",
        BuiltinReceiver::Option => "var receiver: int? = Some(1)\n",
        BuiltinReceiver::Result => "var receiver: Result[int, str] = Ok(1)\n",
    }
}

#[test]
fn every_builtin_method_rejects_the_wrong_arity() {
    for method in BUILTIN_METHODS {
        let mut source = String::from("fn main() {\n");
        source.push_str(receiver_declaration(method.receiver));
        source.push_str("    receiver.");
        source.push_str(method.name);
        source.push('(');
        for index in 0..=method.arity {
            if index > 0 {
                source.push_str(", ");
            }
            source.push('1');
        }
        source.push_str(")\n}\n");
        let found = codes(Case::single(&source));
        assert!(
            found.contains(&Code::E204),
            "{} {} with arity {} should report E204, got {found:?}",
            method.receiver.as_str(),
            method.name,
            method.arity
        );
    }
}

#[test]
fn every_callable_item_rejects_the_wrong_arity() {
    for item in BUILTIN_ITEMS {
        let Some(arity) = item.ty.arity() else {
            continue;
        };
        let mut source = format!("use {}\nfn main() {{\n    let _ = ", item.module);
        source.push_str(if item.module == "std.math" {
            "math."
        } else {
            "json."
        });
        source.push_str(item.name);
        source.push('(');
        for index in 0..=arity {
            if index > 0 {
                source.push_str(", ");
            }
            source.push('1');
        }
        source.push_str(")\n}\n");
        let found = codes(Case::single(&source));
        assert!(
            found.contains(&Code::E204),
            "{}.{} with arity {arity} should report E204, got {found:?}",
            item.module,
            item.name
        );
    }
}

#[test]
fn prelude_functions_reject_the_wrong_arity() {
    for function in BUILTIN_FNS {
        let arity = function.sig.params.len();
        let mut source = String::from("fn main() {\n    ");
        source.push_str(function.name);
        source.push('(');
        for index in 0..=arity {
            if index > 0 {
                source.push_str(", ");
            }
            source.push('1');
        }
        source.push_str(")\n}\n");
        let found = codes(Case::single(&source));
        assert!(
            found.contains(&Code::E204),
            "{} with arity {arity} should report E204, got {found:?}",
            function.name
        );
    }
}
