use martite_ir::{FieldDef, Ty, TyId, TypeDef, TypeDefKind, TypeId, TypeTable, VariantDef};

#[test]
fn definitions_keep_declaration_order_and_support_lookups() {
    let mut table = TypeTable::default();
    let int = table.intern_ty(Ty::Int);
    let point = table.add_def(TypeDef {
        name: "Point".into(),
        kind: TypeDefKind::Struct {
            fields: vec![
                FieldDef {
                    name: "x".into(),
                    ty: int,
                },
                FieldDef {
                    name: "y".into(),
                    ty: int,
                },
            ],
        },
        params: 0,
        is_interface: false,
        is_builtin: false,
    });
    let shape = table.add_def(TypeDef {
        name: "Shape".into(),
        kind: TypeDefKind::Enum {
            variants: vec![
                VariantDef {
                    name: "Dot".into(),
                    payload: Vec::new(),
                },
                VariantDef {
                    name: "Circle".into(),
                    payload: vec![int],
                },
            ],
        },
        params: 0,
        is_interface: false,
        is_builtin: false,
    });

    assert_eq!(point, TypeId::new(0));
    assert_eq!(shape, TypeId::new(1));
    assert_eq!(table.field_index(point, "y"), Some(1));
    assert_eq!(table.field(point, "x").unwrap().ty, int);
    assert_eq!(table.variant_index(shape, "Circle"), Some(1));
    assert_eq!(table.variant(shape, 0).unwrap().name.as_ref(), "Dot");
    assert!(!table.get(point).unwrap().is_opaque());
}

#[test]
fn structural_types_are_interned() {
    let mut table = TypeTable::default();
    let int = table.intern_ty(Ty::Int);
    let same = table.intern_ty(Ty::Int);
    let list = table.intern_ty(Ty::List(int));
    let same_list = table.intern_ty(Ty::List(int));
    let other_list = table.intern_ty(Ty::List(TyId::new(99)));

    assert_eq!(int, same);
    assert_eq!(list, same_list);
    assert_ne!(list, other_list);
    assert_eq!(table.tys.len(), 3);
}
