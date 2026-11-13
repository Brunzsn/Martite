use martite_check::TypeDefKind as CheckTypeDefKind;
use martite_conformance::Case;
use martite_ir::{TypeDefKind, TypeId, TypeTable};

const SOURCE: &str = r#"
struct Pair[A, B] {
    first: A,
    second: B,
}

enum Tree {
    Leaf,
    Node(Tree, int, Tree),
}

fn main() {
    let pair = Pair { first: 1, second: "two" }
    let tree = Tree.Node(Tree.Leaf, 1, Tree.Leaf)
    print("{pair}")
    print("{tree}")
}
"#;

#[test]
fn ir_type_table_mirrors_checker_declaration_order() {
    let loaded = Case::single(SOURCE).load();
    assert!(
        !loaded.has_errors(),
        "fixture must check: {:?}",
        loaded.diagnostics
    );
    let program = loaded.program.expect("checked program");
    let table = TypeTable::from_checker(&program);

    assert_eq!(table.defs.len(), program.types.defs.len());
    assert_eq!(table.get(TypeId::new(0)).unwrap().name.as_ref(), "Json");

    for (index, check_def) in program.types.defs.iter().enumerate() {
        let ir_def = table
            .get(TypeId::new(index as u32))
            .expect("IR definition at the checker index");
        assert_eq!(ir_def.name.as_ref(), check_def.name);
        assert_eq!(ir_def.is_interface, check_def.is_interface);
        assert_eq!(ir_def.is_builtin, check_def.is_builtin);
        assert_eq!(ir_def.params as usize, check_def.params.len());

        match (&check_def.kind, &ir_def.kind) {
            (CheckTypeDefKind::Struct { fields: check }, TypeDefKind::Struct { fields: ir }) => {
                assert_eq!(ir.len(), check.len());
                for (check_field, ir_field) in check.iter().zip(ir.iter()) {
                    assert_eq!(ir_field.name.as_ref(), check_field.name);
                }
            }
            (CheckTypeDefKind::Enum { variants: check }, TypeDefKind::Enum { variants: ir }) => {
                assert_eq!(ir.len(), check.len());
                for (check_variant, ir_variant) in check.iter().zip(ir.iter()) {
                    assert_eq!(ir_variant.name.as_ref(), check_variant.name);
                    assert_eq!(ir_variant.payload.len(), check_variant.payload.len());
                }
            }
            (CheckTypeDefKind::Opaque, TypeDefKind::Opaque) => {}
            _ => panic!("type kind mismatch for {}", check_def.name),
        }
    }
}

#[test]
fn ir_type_table_preserves_recursive_and_generic_references() {
    let loaded = Case::single(SOURCE).load();
    assert!(!loaded.has_errors());
    let program = loaded.program.expect("checked program");
    let table = TypeTable::from_checker(&program);

    let tree = program
        .types
        .defs
        .iter()
        .position(|def| def.name == "Tree")
        .expect("Tree definition");
    let tree_id = TypeId::new(tree as u32);
    let TypeDefKind::Enum { variants } = &table.get(tree_id).unwrap().kind else {
        panic!("Tree must be an enum");
    };
    assert_eq!(variants[1].name.as_ref(), "Node");
    assert_eq!(variants[1].payload.len(), 3);
    assert!(matches!(
        table.get_ty(variants[1].payload[0]),
        Some(martite_ir::Ty::Named(id, args)) if *id == tree_id && args.is_empty()
    ));

    let pair = program
        .types
        .defs
        .iter()
        .position(|def| def.name == "Pair")
        .expect("Pair definition");
    let pair_def = table.get(TypeId::new(pair as u32)).unwrap();
    assert_eq!(pair_def.params, 2);
    let TypeDefKind::Struct { fields } = &pair_def.kind else {
        panic!("Pair must be a struct");
    };
    assert!(matches!(
        table.get_ty(fields[0].ty),
        Some(martite_ir::Ty::Param(0))
    ));
    assert!(matches!(
        table.get_ty(fields[1].ty),
        Some(martite_ir::Ty::Param(1))
    ));
}
