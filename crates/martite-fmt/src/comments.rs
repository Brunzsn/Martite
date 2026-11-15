//! Comment attachment verification for `martite fmt --verify`.
//!
//! Comments are trivia: they carry source spans but are not stored on the AST
//! node they describe. Comparing only a multiset of comment texts would accept
//! a formatter bug that moves `// above a` from one item to another. Instead,
//! every comment is resolved to an *anchor*:
//!
//! - the enclosing or adjacent item, statement, or match arm;
//! - an own-line comment attaches to the next anchor;
//! - a same-line trailing comment attaches to the previous anchor.
//!
//! Blocks and arm lists are scopes rather than anchors, so comments between
//! nested statements stay attached to the inner statement instead of collapsing
//! onto the enclosing `if` or `while`. Import anchors use the same canonical
//! order as the structural comparison, which lets a leading comment travel with
//! its `use` item when the formatter sorts an import run.

use std::collections::BTreeMap;

use martite_base::{Span, normalize_source};
use martite_syntax::{
    Block, Comment, Expr, ExprKind, FieldInit, Item, LambdaBody, MatchArm, MatchBody, Program,
    Stmt, StmtKind, StrExprPart,
};

use crate::normalize;

/// Comments grouped by the anchor they describe.
type AnchoredComments = BTreeMap<usize, Vec<(bool, String)>>;

/// Compare comment placement between two structurally equal programs.
pub(crate) fn comments_equivalent(
    before_source: &str,
    before: &Program,
    after_source: &str,
    after: &Program,
) -> bool {
    let before_source = normalize_source(before_source);
    let after_source = normalize_source(after_source);
    let before_tree = AnchorTree::new(before);
    let after_tree = AnchorTree::new(after);
    anchored_comments(before_source.as_ref(), before, &before_tree)
        == anchored_comments(after_source.as_ref(), after, &after_tree)
}

/// Return items in the order the formatter may emit them. Consecutive `use`
/// items form one run and are sorted by path, matching the formatter and the
/// structural comparison.
pub(crate) fn canonical_items(items: &[Item]) -> Vec<&Item> {
    let mut out = Vec::with_capacity(items.len());
    let mut index = 0;
    while index < items.len() {
        if !matches!(items[index], Item::Use(_)) {
            out.push(&items[index]);
            index += 1;
            continue;
        }

        let start = index;
        while index < items.len() && matches!(items[index], Item::Use(_)) {
            index += 1;
        }
        let mut run: Vec<&Item> = items[start..index].iter().collect();
        run.sort_by(|left, right| use_path(left).cmp(use_path(right)));
        out.extend(run);
    }
    out
}

fn use_path(item: &Item) -> &[String] {
    match item {
        Item::Use(decl) => &decl.path,
        _ => unreachable!("import runs contain only use items"),
    }
}

fn anchored_comments(source: &str, program: &Program, tree: &AnchorTree) -> AnchoredComments {
    let mut comments: AnchoredComments = BTreeMap::new();
    for comment in &program.comments {
        let anchor = tree.anchor_for(source, comment);
        comments
            .entry(anchor)
            .or_default()
            .push((comment.doc, normalize::comment(comment)));
    }
    comments
}

struct Node {
    span: Span,
    anchor: Option<usize>,
    children: Vec<usize>,
}

/// A tree of anchors and scope nodes. The root has anchor ID 0 and is used for
/// comments that do not belong to any item.
struct AnchorTree {
    nodes: Vec<Node>,
    root: usize,
    next_anchor: usize,
}

impl AnchorTree {
    fn new(program: &Program) -> Self {
        let mut tree = Self {
            nodes: vec![Node {
                span: Span::new(0, u32::MAX),
                anchor: None,
                children: Vec::new(),
            }],
            root: 0,
            next_anchor: 1,
        };
        let root = tree.root;
        for item in canonical_items(&program.items) {
            tree.item(item, root);
        }
        tree
    }

    fn anchor_for(&self, source: &str, comment: &Comment) -> usize {
        let trailing = normalize::is_trailing_comment(source, comment);
        self.resolve(self.root, comment, trailing).unwrap_or(0)
    }

    fn resolve(&self, index: usize, comment: &Comment, trailing: bool) -> Option<usize> {
        let node = &self.nodes[index];
        if let Some(&child) = node
            .children
            .iter()
            .find(|&&child| self.nodes[child].span.contains(comment.span))
        {
            return self.resolve(child, comment, trailing).or(node.anchor);
        }

        if trailing {
            if let Some(anchor) = self.adjacent_anchor(index, comment, true) {
                return Some(anchor);
            }
        } else if let Some(anchor) = self.adjacent_anchor(index, comment, false) {
            return Some(anchor);
        }

        // A comment before the first child or after the last child still belongs
        // to that child, not to the enclosing container.
        self.adjacent_anchor(index, comment, !trailing)
            .or(node.anchor)
    }

    fn adjacent_anchor(&self, index: usize, comment: &Comment, previous: bool) -> Option<usize> {
        let mut best: Option<(u32, usize)> = None;
        for &child in &self.nodes[index].children {
            let node = &self.nodes[child];
            let Some(anchor) = node.anchor else {
                continue;
            };
            if previous {
                if node.span.end > comment.span.start {
                    continue;
                }
                if best.is_none_or(|(position, _)| node.span.end > position) {
                    best = Some((node.span.end, anchor));
                }
            } else {
                if node.span.start < comment.span.end {
                    continue;
                }
                if best.is_none_or(|(position, _)| node.span.start < position) {
                    best = Some((node.span.start, anchor));
                }
            }
        }
        best.map(|(_, anchor)| anchor)
    }

    fn item(&mut self, item: &Item, parent: usize) {
        let node = self.push_anchor(item.span(), parent);
        match item {
            Item::Test(decl) => self.block(&decl.body, node),
            Item::On(decl) => self.block(&decl.body, node),
            Item::Fn(decl) => {
                if let Some(body) = &decl.body {
                    self.block(body, node);
                }
            }
            Item::Impl(decl) => {
                for method in &decl.methods {
                    if let Some(body) = &method.body {
                        self.block(body, node);
                    }
                }
            }
            Item::Use(_) | Item::Struct(_) | Item::Enum(_) | Item::Const(_) => {}
        }
    }

    fn block(&mut self, block: &Block, parent: usize) {
        let node = self.push(block.span, None, parent);
        for stmt in &block.stmts {
            self.stmt(stmt, node);
        }
    }

    fn stmt(&mut self, stmt: &Stmt, parent: usize) {
        let node = self.push_anchor(stmt.span, parent);
        match &stmt.kind {
            StmtKind::Let { value, .. } => self.expr(value, node),
            StmtKind::While { cond, body } => {
                self.expr(cond, node);
                self.block(body, node);
            }
            StmtKind::For { iter, body, .. } => {
                self.expr(iter, node);
                self.block(body, node);
            }
            StmtKind::Return(Some(expr)) => self.expr(expr, node),
            StmtKind::Return(None) | StmtKind::Break | StmtKind::Continue => {}
            StmtKind::Expr { expr, assign } => {
                self.expr(expr, node);
                if let Some((_, rhs)) = assign {
                    self.expr(rhs, node);
                }
            }
        }
    }

    fn expr(&mut self, expr: &Expr, parent: usize) {
        match &expr.kind {
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Unit
            | ExprKind::Var(_) => {}
            ExprKind::Str(parts) => {
                for part in parts {
                    if let StrExprPart::Expr(expr) = part {
                        self.expr(expr, parent);
                    }
                }
            }
            ExprKind::Tuple(items) | ExprKind::List(items) => {
                for item in items {
                    self.expr(item, parent);
                }
            }
            ExprKind::Map(entries) => {
                for (key, value) in entries {
                    self.expr(key, parent);
                    self.expr(value, parent);
                }
            }
            ExprKind::StructLit { fields, base, .. } => {
                self.field_inits(fields, parent);
                if let Some(base) = base {
                    self.expr(base, parent);
                }
            }
            ExprKind::Unary { expr, .. } | ExprKind::Try(expr) | ExprKind::Convert { expr, .. } => {
                self.expr(expr, parent)
            }
            ExprKind::Binary { lhs, rhs, .. } => {
                self.expr(lhs, parent);
                self.expr(rhs, parent);
            }
            ExprKind::Range { start, end, .. } => {
                self.expr(start, parent);
                self.expr(end, parent);
            }
            ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.expr(cond, parent);
                self.block(then_branch, parent);
                if let Some(else_branch) = else_branch {
                    self.expr(else_branch, parent);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.expr(scrutinee, parent);
                // Keep scrutinee comments on the enclosing statement/arm. The
                // arm scope starts after the scrutinee so only comments in the
                // arm list attach to an arm.
                let scope = self.push(Span::new(scrutinee.span.end, expr.span.end), None, parent);
                for arm in arms {
                    self.arm(arm, scope);
                }
            }
            ExprKind::Lambda { body, .. } => match body {
                LambdaBody::Block(block) => self.block(block, parent),
                LambdaBody::Expr(expr) => self.expr(expr, parent),
            },
            ExprKind::Block(block) => self.block(block, parent),
            ExprKind::Field { base, .. } => self.expr(base, parent),
            ExprKind::Call { callee, args } => {
                self.expr(callee, parent);
                for arg in args {
                    self.expr(&arg.expr, parent);
                }
            }
            ExprKind::Index { base, index } => {
                self.expr(base, parent);
                self.expr(index, parent);
            }
        }
    }

    fn field_inits(&mut self, fields: &[FieldInit], parent: usize) {
        for field in fields {
            if let Some(value) = &field.value {
                self.expr(value, parent);
            }
        }
    }

    fn arm(&mut self, arm: &MatchArm, parent: usize) {
        let node = self.push_anchor(arm.span, parent);
        if let Some(guard) = &arm.guard {
            self.expr(guard, node);
        }
        match &arm.body {
            MatchBody::Block(block) => self.block(block, node),
            MatchBody::Expr(expr) => self.expr(expr, node),
        }
    }

    fn push_anchor(&mut self, span: Span, parent: usize) -> usize {
        let anchor = self.next_anchor;
        self.next_anchor += 1;
        self.push(span, Some(anchor), parent)
    }

    fn push(&mut self, span: Span, anchor: Option<usize>, parent: usize) -> usize {
        let index = self.nodes.len();
        self.nodes.push(Node {
            span,
            anchor,
            children: Vec::new(),
        });
        self.nodes[parent].children.push(index);
        index
    }
}
