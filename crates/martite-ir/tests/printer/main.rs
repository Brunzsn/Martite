mod fixtures;

use std::path::{Path, PathBuf};

use fixtures::{FnBuilder, ProgramBuilder};
use martite_ir::{Charge, Effects, FnKind, NodeKind, Printer, TyId};

fn golden_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/printer/golden")
}

#[test]
fn printer_goldens_match() {
    for (name, program) in fixtures::all() {
        let path = golden_root().join(format!("{name}.ir"));
        let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!(
                "missing golden {}; run:\n  cargo test -p martite-ir --test printer -- --ignored update_printer_goldens",
                path.display()
            )
        });
        assert_eq!(
            program.print(),
            expected,
            "printer golden mismatch for {name}"
        );
    }
}

#[test]
#[ignore = "regenerates the checked-in IR printer goldens"]
fn update_printer_goldens() {
    let root = golden_root();
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create golden directory");
    for (name, program) in fixtures::all() {
        std::fs::write(root.join(format!("{name}.ir")), program.print())
            .expect("write printer golden");
    }
}

#[test]
fn printing_is_deterministic() {
    for (name, program) in fixtures::all() {
        assert_eq!(program.print(), program.print(), "unstable dump for {name}");
    }
}

#[test]
fn display_matches_print() {
    for (name, program) in fixtures::all() {
        assert_eq!(format!("{program}"), program.print(), "display for {name}");
    }
}

#[test]
fn function_dump_is_part_of_the_program_dump() {
    for (name, program) in fixtures::all() {
        let printer = Printer::new(&program);
        let expected = program.print();
        for function in &program.fns {
            assert!(
                expected.contains(&printer.function(function)),
                "function dump for {name} is missing from the program dump"
            );
        }
    }
}

#[test]
fn unreachable_nodes_are_still_listed() {
    let mut function = FnBuilder::new(0, "main", FnKind::Free, 0);
    let reachable = function.node(NodeKind::Unit, TyId(1), Charge::ZERO, Effects::NONE);
    let orphan = function.node(NodeKind::Unit, TyId(1), Charge::ZERO, Effects::NONE);
    let program = ProgramBuilder::new("orphan.mt")
        .function(function.body(reachable))
        .build();

    let dump = program.print();
    assert!(dump.contains(&format!("    #{} unit", orphan.0)));
    assert!(dump.contains(&format!("body=#{}", reachable.0)));
}
