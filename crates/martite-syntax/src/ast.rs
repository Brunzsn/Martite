use martite_base::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u32);

/// Dense side table indexed by [`NodeId`].
///
/// Slots live in a `Vec`, so lookups are a bounds check instead of a hash
/// lookup. IDs must come from the same parse session as the program that owns
/// the table; that keeps the indexed range dense.
#[derive(Clone, PartialEq)]
pub struct NodeTable<T> {
    slots: Vec<Option<T>>,
}

impl<T> Default for NodeTable<T> {
    fn default() -> Self {
        Self { slots: Vec::new() }
    }
}

impl<T> NodeTable<T> {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            slots: Vec::with_capacity(capacity),
        }
    }

    pub fn insert(&mut self, id: NodeId, value: T) -> Option<T> {
        let index = id.0 as usize;
        if self.slots.len() <= index {
            self.slots.resize_with(index + 1, || None);
        }
        self.slots[index].replace(value)
    }

    #[inline]
    pub fn get(&self, id: &NodeId) -> Option<&T> {
        self.slots.get(id.0 as usize).and_then(Option::as_ref)
    }

    #[inline]
    pub fn get_mut(&mut self, id: &NodeId) -> Option<&mut T> {
        self.slots.get_mut(id.0 as usize).and_then(Option::as_mut)
    }

    pub fn contains_key(&self, id: &NodeId) -> bool {
        self.get(id).is_some()
    }

    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.slots.iter().filter_map(Option::as_ref)
    }

    pub fn iter(&self) -> impl Iterator<Item = (NodeId, &T)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(index, value)| value.as_ref().map(|value| (NodeId(index as u32), value)))
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for NodeTable<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

/// One pair of source parentheses around a node. Nested pairs are recorded
/// innermost first, so the formatter can reproduce redundant `((x))` too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParenPair {
    pub open: Span,
    pub close: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Use(UseDecl),
    Test(TestDecl),
    On(OnDecl),
    Impl(ImplDecl),
    Fn(FnDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
    Const(ConstDecl),
}

impl Item {
    pub fn id(&self) -> NodeId {
        match self {
            Item::Use(i) => i.id,
            Item::Test(i) => i.id,
            Item::On(i) => i.id,
            Item::Impl(i) => i.id,
            Item::Fn(i) => i.id,
            Item::Struct(i) => i.id,
            Item::Enum(i) => i.id,
            Item::Const(i) => i.id,
        }
    }

    pub fn span(&self) -> Span {
        match self {
            Item::Use(i) => i.span,
            Item::Test(i) => i.span,
            Item::On(i) => i.span,
            Item::Impl(i) => i.span,
            Item::Fn(i) => i.span,
            Item::Struct(i) => i.span,
            Item::Enum(i) => i.span,
            Item::Const(i) => i.span,
        }
    }

    pub fn name(&self) -> Option<&str> {
        match self {
            Item::Use(_) | Item::Test(_) | Item::Impl(_) => None,
            Item::On(i) => Some(&i.name),
            Item::Fn(i) => Some(&i.name),
            Item::Struct(i) => Some(&i.name),
            Item::Enum(i) => Some(&i.name),
            Item::Const(i) => Some(&i.name),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UseDecl {
    pub id: NodeId,
    pub span: Span,
    pub path: Vec<String>,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TestDecl {
    pub id: NodeId,
    pub span: Span,
    pub name: String,
    pub body: Block,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OnDecl {
    pub id: NodeId,
    pub span: Span,
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Option<TyExpr>,
    pub body: Block,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FnDecl {
    pub id: NodeId,
    pub span: Span,
    pub is_pub: bool,
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub params: Vec<Param>,
    pub ret: Option<TyExpr>,
    /// `None` in an interface file.
    pub body: Option<Block>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImplDecl {
    pub id: NodeId,
    pub span: Span,
    pub generics: Vec<GenericParam>,
    pub type_name: String,
    pub type_args: Vec<TyExpr>,
    pub methods: Vec<FnDecl>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructDecl {
    pub id: NodeId,
    pub span: Span,
    pub is_pub: bool,
    pub name: String,
    pub generics: Vec<GenericParam>,
    /// Empty and `opaque = true` for an opaque handle.
    pub fields: Vec<Field>,
    pub opaque: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumDecl {
    pub id: NodeId,
    pub span: Span,
    pub is_pub: bool,
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub variants: Vec<Variant>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConstDecl {
    pub id: NodeId,
    pub span: Span,
    pub is_pub: bool,
    pub name: String,
    pub ty: TyExpr,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GenericParam {
    pub id: NodeId,
    pub span: Span,
    pub name: String,
    pub bounds: Vec<Bound>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    Eq,
    Ord,
    Show,
}

impl Bound {
    pub const fn as_str(self) -> &'static str {
        match self {
            Bound::Eq => "Eq",
            Bound::Ord => "Ord",
            Bound::Show => "Show",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub id: NodeId,
    pub span: Span,
    pub kind: ParamKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParamKind {
    SelfParam {
        mutable: bool,
    },
    Normal {
        name: String,
        mutable: bool,
        ty: TyExpr,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub id: NodeId,
    pub span: Span,
    pub name: String,
    pub ty: TyExpr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    pub id: NodeId,
    pub span: Span,
    pub name: String,
    pub payload: Vec<TyExpr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TyExpr {
    pub id: NodeId,
    pub span: Span,
    pub kind: TyKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TyKind {
    Prim(PrimTy),
    Unit,
    Named {
        name: String,
        args: Vec<TyExpr>,
    },
    Optional(Box<TyExpr>),
    Tuple(Vec<TyExpr>),
    Fn {
        params: Vec<TyExpr>,
        ret: Option<Box<TyExpr>>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimTy {
    Int,
    Float,
    Bool,
    Str,
    Range,
}

impl PrimTy {
    pub const fn as_str(self) -> &'static str {
        match self {
            PrimTy::Int => "int",
            PrimTy::Float => "float",
            PrimTy::Bool => "bool",
            PrimTy::Str => "str",
            PrimTy::Range => "range",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub id: NodeId,
    pub span: Span,
    pub stmts: Vec<Stmt>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub id: NodeId,
    pub span: Span,
    pub kind: StmtKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    Let {
        mutable: bool,
        pattern: Pattern,
        ty: Option<TyExpr>,
        value: Expr,
    },
    While {
        cond: Expr,
        body: Block,
    },
    For {
        pattern: Pattern,
        iter: Expr,
        body: Block,
    },
    Return(Option<Expr>),
    Break,
    Continue,
    Expr {
        expr: Expr,
        assign: Option<(AssignOp, Expr)>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    Assign,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub id: NodeId,
    pub span: Span,
    pub kind: ExprKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(Vec<StrExprPart>),
    Unit,
    Var(String),
    Tuple(Vec<Expr>),
    List(Vec<Expr>),
    Map(Vec<(Expr, Expr)>),
    StructLit {
        name: String,
        fields: Vec<FieldInit>,
        base: Option<Box<Expr>>,
    },
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Range {
        start: Box<Expr>,
        end: Box<Expr>,
        inclusive: bool,
    },
    If {
        cond: Box<Expr>,
        then_branch: Block,
        else_branch: Option<Box<Expr>>,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    Lambda {
        params: Vec<LambdaParam>,
        ret: Option<TyExpr>,
        body: LambdaBody,
    },
    Block(Block),
    Field {
        base: Box<Expr>,
        name: FieldName,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Arg>,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    Try(Box<Expr>),
    Convert {
        to: PrimTy,
        expr: Box<Expr>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum StrExprPart {
    Text(String),
    Expr(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldInit {
    pub id: NodeId,
    pub span: Span,
    pub name: String,
    pub value: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    pub id: NodeId,
    pub span: Span,
    pub mutable: bool,
    pub expr: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldName {
    Ident(String),
    Index(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Coalesce,
}

impl BinaryOp {
    pub const fn as_str(self) -> &'static str {
        match self {
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Rem => "%",
            BinaryOp::Eq => "==",
            BinaryOp::Ne => "!=",
            BinaryOp::Lt => "<",
            BinaryOp::Le => "<=",
            BinaryOp::Gt => ">",
            BinaryOp::Ge => ">=",
            BinaryOp::And => "and",
            BinaryOp::Or => "or",
            BinaryOp::Coalesce => "??",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LambdaParam {
    pub id: NodeId,
    pub span: Span,
    pub pattern: Pattern,
    pub ty: Option<TyExpr>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LambdaBody {
    Block(Block),
    Expr(Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub id: NodeId,
    pub span: Span,
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: MatchBody,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MatchBody {
    Block(Block),
    Expr(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    pub id: NodeId,
    pub span: Span,
    pub kind: PatKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PatKind {
    Wild,
    Bind(String),
    Int(i64),
    Bool(bool),
    Str(String),
    Tuple(Vec<Pattern>),
    Variant {
        qualifier: Option<String>,
        name: String,
        args: Vec<Pattern>,
    },
    Struct {
        name: String,
        fields: Vec<FieldPat>,
        rest: bool,
    },
    Or(Vec<Pattern>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldPat {
    pub id: NodeId,
    pub span: Span,
    pub name: String,
    pub pattern: Pattern,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_table_indexes_by_dense_id() {
        let mut table = NodeTable::default();
        assert_eq!(table.get(&NodeId(2)), None);
        assert_eq!(table.insert(NodeId(2), "two"), None);
        assert_eq!(table.insert(NodeId(1), "one"), None);
        assert_eq!(table.get(&NodeId(1)), Some(&"one"));
        assert_eq!(table.get(&NodeId(2)), Some(&"two"));
        assert_eq!(table.get(&NodeId(3)), None);
        assert_eq!(table.values().copied().collect::<Vec<_>>(), ["one", "two"]);
        assert_eq!(table.insert(NodeId(2), "second"), Some("two"));
        assert_eq!(table.get(&NodeId(2)), Some(&"second"));
    }
}
