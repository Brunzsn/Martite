//! Generated-program property tests for the IR pipeline (#113).
//!
//! The proptest case builds a well-typed, host-free source from a dense opcode
//! stream; the seed replay test exercises the checked-in minimized corpus
//! without requiring `cargo-fuzz`.

use std::path::Path;

use martite_conformance::Case;
use martite_conformance::property::{
    PROPERTY_CASES, PropertyOutcome, check_source, check_trapping_case, generate_program,
};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig {
        cases: PROPERTY_CASES,
        max_shrink_iters: 512,
        ..ProptestConfig::default()
    })]

    #[test]
    fn generated_programs_lower_verify_and_execute(ops in prop::collection::vec(any::<u64>(), 1..=64)) {
        let source = generate_program(&ops);
        if let Err(failure) = check_source(&source) {
            prop_assert!(false, "ops = {ops:?}\n{failure}");
        }
    }
}

#[test]
fn generator_only_emits_host_free_programs() {
    for ops in [
        Vec::new(),
        vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
        vec![u64::MAX; 64],
    ] {
        let source = generate_program(&ops);
        assert!(
            source.contains("fn main()"),
            "generated program has no main"
        );
        for forbidden in ["std.io", "std.env", "use ", "on ", "panic(", "assert("] {
            assert!(
                !source.contains(forbidden),
                "generator emitted forbidden construct `{forbidden}`:\n{source}"
            );
        }
    }
}

#[test]
fn checked_in_trapping_lambda_reports_the_defining_source() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/property/trapping_lambda");
    let main = std::fs::read_to_string(root.join("main.mt")).expect("read trap seed main.mt");
    let util = std::fs::read_to_string(root.join("util.mt")).expect("read trap seed util.mt");
    let case = Case::single(main).file("util.mt", util);
    let trap = check_trapping_case(&case).expect("trapping lambda seed");
    assert_eq!(trap.source.as_deref(), Some("util.mt"));
    assert_eq!(trap.text.as_deref(), Some("panic(\"boom\")"));
    assert_eq!(trap.trace, vec!["main".to_string(), "<lambda>".to_string()]);
}

#[test]
fn checked_in_property_seeds_replay() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/property");
    let mut paths: Vec<_> = std::fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("read {}: {error}", root.display()))
        .map(|entry| entry.expect("property seed entry").path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("mt"))
        .collect();
    paths.sort();
    assert!(
        paths.len() >= 6,
        "expected at least six checked-in property seeds in {}",
        root.display()
    );

    for path in paths {
        let source = std::fs::read_to_string(&path).expect("read property seed");
        match check_source(&source) {
            Ok(PropertyOutcome::Passed { .. }) => {}
            Ok(PropertyOutcome::Skipped { reason }) => {
                panic!("{}: skipped: {reason}", path.display())
            }
            Err(failure) => panic!("{}: {failure}", path.display()),
        }
    }
}
