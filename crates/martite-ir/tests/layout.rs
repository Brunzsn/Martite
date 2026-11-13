use std::mem::size_of;

use martite_ir::{
    BuiltinFnId, BuiltinItemId, BuiltinMethodId, CaptureIndex, Charge, ConstId, Effects, FnId,
    HostId, IrId, MatchSetId, Node, NodeKind, PatternId, Slot, StrId, StructLayoutId, TyId, TypeId,
};

#[test]
fn node_stays_within_the_cache_budget() {
    assert_eq!(size_of::<Node>(), 32);
    assert_eq!(size_of::<NodeKind>(), 12);
    assert_eq!(size_of::<Charge>(), 12);
    assert_eq!(size_of::<Effects>(), 4);
}

#[test]
fn arena_ids_are_u32_indices() {
    assert_eq!(size_of::<FnId>(), 4);
    assert_eq!(size_of::<IrId>(), 4);
    assert_eq!(size_of::<ConstId>(), 4);
    assert_eq!(size_of::<StrId>(), 4);
    assert_eq!(size_of::<HostId>(), 4);
    assert_eq!(size_of::<Slot>(), 4);
    assert_eq!(size_of::<TyId>(), 4);
    assert_eq!(size_of::<TypeId>(), 4);
    assert_eq!(size_of::<CaptureIndex>(), 4);
    assert_eq!(size_of::<PatternId>(), 4);
    assert_eq!(size_of::<MatchSetId>(), 4);
    assert_eq!(size_of::<StructLayoutId>(), 4);
    assert_eq!(size_of::<BuiltinFnId>(), 4);
    assert_eq!(size_of::<BuiltinItemId>(), 4);
    assert_eq!(size_of::<BuiltinMethodId>(), 4);
}
