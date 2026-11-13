//! Deterministic text dumps of [`IrProgram`].
//!
//! The output is a flat arena dump: every function is printed in arena order,
//! every node in node order, and all cross-references are explicit ids. The
//! format is intended for lowering review, verifier debugging, optimizer work
//! and golden tests. It never iterates a hash map, aligns columns or prints a
//! pointer, so equal IR always produces byte-identical text.
//!
//! Every node line has the same tail:
//!
//! ```text
//! #12 add lhs=#10 rhs=#11 ty=ty#0 charge=1/0/0 effects=MAY_TRAP span=src#0:20..25 safepoint=0 origin=-
//! ```
//!
//! `charge` is `pre/post/per_iter`, `effects` uses the flag order from
//! [`crate::Effects`], `span` carries the defining function's source id,
//! `safepoint` is `1` when this exact node performs a safepoint check, and
//! `origin` is the synthetic node's source id or `-`. The function header also
//! carries `ret=ty#N`.

use std::fmt::{self, Write};

use martite_base::{SourceId, Span};

use crate::id::{ConstId, FnId, IrId, StrId};
use crate::node::{AccessMode, ChildRange, NodeKind, StrPart};
use crate::pattern::Pattern;
use crate::program::{
    CaptureInfo, ConstValue, FnKind, HookSig, HostFnInfo, IrFn, IrProgram, ParamInfo,
    PreludeVariant, SlotInfo,
};
use crate::types::{TyId, TypeId};

/// Version of the text format emitted by [`Printer`].
pub const FORMAT_VERSION: u32 = 1;

/// Writes deterministic text dumps for one [`IrProgram`].
pub struct Printer<'a> {
    program: &'a IrProgram,
}

impl<'a> Printer<'a> {
    pub const fn new(program: &'a IrProgram) -> Self {
        Self { program }
    }

    /// The complete program dump.
    pub fn program(&self) -> String {
        let mut out = String::new();
        self.write_program(&mut out)
            .expect("writing to a String cannot fail");
        out
    }

    /// Write the complete program dump.
    pub fn write_program(&self, out: &mut impl Write) -> fmt::Result {
        writeln!(out, "ir-format {FORMAT_VERSION}")?;
        self.write_sources(out)?;
        self.write_grants(out)?;
        self.write_hosts(out)?;
        self.write_entries(out)?;
        self.write_consts(out)?;
        self.write_strings(out)?;
        for function in &self.program.fns {
            writeln!(out)?;
            self.write_function(function, out)?;
        }
        Ok(())
    }

    /// One function's dump.
    pub fn function(&self, function: &IrFn) -> String {
        let mut out = String::new();
        self.write_function(function, &mut out)
            .expect("writing to a String cannot fail");
        out
    }

    /// Write one function's dump.
    pub fn write_function(&self, function: &IrFn, out: &mut impl Write) -> fmt::Result {
        write!(out, "fn #{} ", function.id.0)?;
        write_quoted(out, &function.name)?;
        out.write_str(" kind=")?;
        write_fn_kind(out, function.kind)?;
        write!(out, " source=src#{}", function.source.0)?;
        if let Some(name) = self.program.source_name(function.source) {
            out.write_char('(')?;
            write_quoted(out, name)?;
            out.write_char(')')?;
        }
        write!(
            out,
            " slots={} ret=ty#{} body=",
            function.locals, function.ret.0
        )?;
        match function.body {
            Some(body) => writeln!(out, "#{}", body.0)?,
            None => out.write_str("-\n")?,
        }

        self.write_params(out, &function.params)?;
        self.write_slots(out, &function.slot_info)?;
        self.write_captures(out, &function.captures)?;
        self.write_patterns(out, &function.patterns)?;
        self.write_matches(out, &function.matches)?;
        self.write_layouts(out, &function.struct_layouts)?;
        self.write_str_parts(out, &function.str_parts)?;
        self.write_nodes(out, function)
    }

    fn write_sources(&self, out: &mut impl Write) -> fmt::Result {
        out.write_str("sources:\n")?;
        if self.program.source_names.is_empty() {
            return out.write_str("  (none)\n");
        }
        for (index, name) in self.program.source_names.iter().enumerate() {
            write!(out, "  #{index} ")?;
            write_quoted(out, name)?;
            out.write_char('\n')?;
        }
        Ok(())
    }

    fn write_grants(&self, out: &mut impl Write) -> fmt::Result {
        out.write_str("grants:\n")?;
        if self.program.grants.is_empty() {
            return out.write_str("  (none)\n");
        }
        for grant in &self.program.grants {
            out.write_str("  ")?;
            write_quoted(out, grant)?;
            out.write_char('\n')?;
        }
        Ok(())
    }

    fn write_hosts(&self, out: &mut impl Write) -> fmt::Result {
        out.write_str("hosts:\n")?;
        if self.program.hosts.is_empty() {
            return out.write_str("  (none)\n");
        }
        for (index, host) in self.program.hosts.iter().enumerate() {
            write!(out, "  #{index} ")?;
            self.write_host(out, host)?;
            out.write_char('\n')?;
        }
        Ok(())
    }

    fn write_host(&self, out: &mut impl Write, host: &HostFnInfo) -> fmt::Result {
        out.write_str("name=")?;
        write_quoted(out, &host.name)?;
        out.write_str(" module=")?;
        write_quoted(out, &host.module)?;
        out.write_str(" params=")?;
        write_tys(out, &host.params)?;
        write!(out, " ret=ty#{} opaque=", host.ret.0)?;
        match host.opaque_return {
            Some(type_id) => write!(out, "type#{}", type_id.0),
            None => out.write_str("-"),
        }
    }

    fn write_entries(&self, out: &mut impl Write) -> fmt::Result {
        let entries = &self.program.entries;
        out.write_str("entries:\n")?;
        writeln!(out, "  kind={}", entry_kind_name(entries.entry_kind))?;
        write_opt_fn(out, "main", entries.main)?;
        out.write_str("  tests=")?;
        write_fn_list(out, &entries.tests)?;
        out.write_char('\n')?;
        out.write_str("  handlers=[")?;
        for (index, (name, function)) in entries.handlers.iter().enumerate() {
            if index != 0 {
                out.write_str(", ")?;
            }
            write_quoted(out, name)?;
            write!(out, " => fn#{}", function.0)?;
        }
        out.write_str("]\n")?;
        out.write_str("  hooks=[")?;
        for (index, (name, hook)) in entries.hooks.iter().enumerate() {
            if index != 0 {
                out.write_str(", ")?;
            }
            write_quoted(out, name)?;
            out.write_str(" => ")?;
            write_hook(out, hook)?;
        }
        out.write_str("]\n")?;
        write_opt_fn(out, "init", entries.init)?;
        write_opt_fn(out, "shutdown", entries.shutdown)?;
        write_opt_ty(out, "state", entries.state_type)?;
        write_opt_type(out, "json", entries.json_type)
    }

    fn write_consts(&self, out: &mut impl Write) -> fmt::Result {
        out.write_str("consts:\n")?;
        if self.program.consts.is_empty() {
            return out.write_str("  (none)\n");
        }
        for (index, value) in self.program.consts.iter().enumerate() {
            write!(out, "  #{index} ")?;
            self.write_const(out, value)?;
            out.write_char('\n')?;
        }
        Ok(())
    }

    fn write_strings(&self, out: &mut impl Write) -> fmt::Result {
        out.write_str("strings:\n")?;
        if self.program.strs.is_empty() {
            return out.write_str("  (none)\n");
        }
        for (index, text) in self.program.strs.iter().enumerate() {
            write!(out, "  #{index} ")?;
            write_quoted(out, text)?;
            out.write_char('\n')?;
        }
        Ok(())
    }

    fn write_params(&self, out: &mut impl Write, params: &[ParamInfo]) -> fmt::Result {
        if params.is_empty() {
            return Ok(());
        }
        out.write_str("  params:\n")?;
        for param in params {
            write!(out, "    slot=#{} name=", param.slot.0)?;
            write_quoted(out, &param.name)?;
            write!(
                out,
                " ty=ty#{} mut={} self={} pattern=",
                param.ty.0,
                yes_no(param.mutable),
                yes_no(param.is_self)
            )?;
            match param.pattern {
                Some(pattern) => writeln!(out, "#{}", pattern.0)?,
                None => out.write_str("-\n")?,
            }
        }
        Ok(())
    }

    fn write_slots(&self, out: &mut impl Write, slots: &[SlotInfo]) -> fmt::Result {
        if slots.is_empty() {
            return Ok(());
        }
        out.write_str("  slots:\n")?;
        for (index, slot) in slots.iter().enumerate() {
            write!(
                out,
                "    #{} ty=ty#{} mut={} name=",
                index,
                slot.ty.0,
                yes_no(slot.mutable)
            )?;
            match &slot.name {
                Some(name) => {
                    write_quoted(out, name)?;
                    out.write_char('\n')?;
                }
                None => out.write_str("-\n")?,
            }
        }
        Ok(())
    }

    fn write_captures(&self, out: &mut impl Write, captures: &[CaptureInfo]) -> fmt::Result {
        if captures.is_empty() {
            return Ok(());
        }
        out.write_str("  captures:\n")?;
        for (index, capture) in captures.iter().enumerate() {
            write!(out, "    #{} ", index)?;
            match capture.source {
                crate::CaptureSource::Slot(slot) => write!(out, "from=slot#{} ", slot.0)?,
                crate::CaptureSource::Capture(capture) => {
                    write!(out, "from=capture#{} ", capture.0)?
                }
            }
            write!(out, "ty=ty#{} name=", capture.ty.0)?;
            match &capture.name {
                Some(name) => {
                    write_quoted(out, name)?;
                    out.write_char('\n')?;
                }
                None => out.write_str("-\n")?,
            }
        }
        Ok(())
    }

    fn write_patterns(&self, out: &mut impl Write, patterns: &[Pattern]) -> fmt::Result {
        if patterns.is_empty() {
            return Ok(());
        }
        out.write_str("  patterns:\n")?;
        for (index, pattern) in patterns.iter().enumerate() {
            write!(out, "    #{index} ")?;
            self.write_pattern(out, pattern)?;
            out.write_char('\n')?;
        }
        Ok(())
    }

    fn write_matches(
        &self,
        out: &mut impl Write,
        matches: &[crate::pattern::MatchSet],
    ) -> fmt::Result {
        if matches.is_empty() {
            return Ok(());
        }
        out.write_str("  matches:\n")?;
        for (index, match_set) in matches.iter().enumerate() {
            writeln!(out, "    #{index}")?;
            for arm in &match_set.arms {
                write!(out, "      arm pattern=#{} guard=", arm.pattern.0)?;
                match arm.guard {
                    Some(guard) => write!(out, "#{}", guard.0)?,
                    None => out.write_str("-")?,
                }
                writeln!(out, " body=#{}", arm.body.0)?;
            }
        }
        Ok(())
    }

    fn write_layouts(
        &self,
        out: &mut impl Write,
        layouts: &[crate::pattern::StructLayout],
    ) -> fmt::Result {
        if layouts.is_empty() {
            return Ok(());
        }
        out.write_str("  layouts:\n")?;
        for (index, layout) in layouts.iter().enumerate() {
            write!(
                out,
                "    #{index} first_child={} fields=",
                layout.first_child
            )?;
            write_u32_list(out, &layout.fields)?;
            out.write_char('\n')?;
        }
        Ok(())
    }

    fn write_str_parts(&self, out: &mut impl Write, parts: &[StrPart]) -> fmt::Result {
        if parts.is_empty() {
            return Ok(());
        }
        out.write_str("  str_parts:\n")?;
        for (index, part) in parts.iter().enumerate() {
            write!(out, "    #{index} ")?;
            self.write_str_part(out, part)?;
            out.write_char('\n')?;
        }
        Ok(())
    }

    fn write_str_part(&self, out: &mut impl Write, part: &StrPart) -> fmt::Result {
        match part {
            StrPart::Text(id) => {
                out.write_str("text ")?;
                write_str_id(out, self.program, *id)
            }
            StrPart::Expr(id) => write!(out, "expr #{}", id.0),
        }
    }

    fn write_nodes(&self, out: &mut impl Write, function: &IrFn) -> fmt::Result {
        if function.nodes.is_empty() {
            return Ok(());
        }
        out.write_str("  nodes:\n")?;
        for (index, node) in function.nodes.iter().enumerate() {
            let id = IrId::new(index as u32);
            write!(out, "    #{} ", id.0)?;
            self.write_kind(out, function, &node.kind)?;
            write!(
                out,
                " ty=ty#{} charge={}/{}/{} effects=",
                node.ty.0, node.charge.pre, node.charge.post, node.charge.per_iter
            )?;
            write_effects(out, node.effects)?;
            out.write_str(" span=")?;
            match function.span(id) {
                Some(span) => write_span(out, function.source, span)?,
                None => out.write_str("-")?,
            }
            out.write_str(" safepoint=")?;
            match function.safepoints.get(index) {
                Some(true) => out.write_str("1")?,
                Some(false) => out.write_str("0")?,
                None => out.write_str("-")?,
            }
            out.write_str(" origin=")?;
            match function.synthetic_origin(id) {
                Some(origin) => write!(out, "#{}", origin.0)?,
                None => out.write_str("-")?,
            }
            out.write_char('\n')?;
        }
        Ok(())
    }

    fn write_kind(&self, out: &mut impl Write, function: &IrFn, kind: &NodeKind) -> fmt::Result {
        match kind {
            NodeKind::Unit => out.write_str("unit"),
            NodeKind::Int(id) => {
                out.write_str("int ")?;
                self.write_const_ref(out, *id)
            }
            NodeKind::Float(id) => {
                out.write_str("float ")?;
                self.write_const_ref(out, *id)
            }
            NodeKind::Bool(value) => write!(out, "bool value={value}"),
            NodeKind::Str(id) => write_str_id(out, self.program, *id),
            NodeKind::ConstRef(id) => {
                out.write_str("const_ref ")?;
                self.write_const_ref(out, *id)
            }
            NodeKind::FnRef(id) => write!(out, "fn_ref fn=fn#{}", id.0),
            NodeKind::BuiltinFnRef(id) => write!(out, "builtin_fn_ref builtin=#{}", id.0),
            NodeKind::BuiltinModuleRef(id) => {
                write!(out, "builtin_module_ref item=#{}", id.0)
            }
            NodeKind::SlotRead { slot, mode } => {
                write!(
                    out,
                    "slot_read slot=#{} mode={}",
                    slot.0,
                    access_mode(*mode)
                )
            }
            NodeKind::CaptureRead(index) => write!(out, "capture_read capture=#{}", index.0),
            NodeKind::SlotPlace(slot) => write!(out, "slot_place slot=#{}", slot.0),
            NodeKind::SlotWrite { slot, value } => {
                write!(out, "slot_write slot=#{} value=#{}", slot.0, value.0)
            }
            NodeKind::SlotInit { slot, value } => {
                write!(out, "slot_init slot=#{} value=#{}", slot.0, value.0)
            }
            NodeKind::Field { base, index } => {
                write!(out, "field base=#{} index={index}", base.0)
            }
            NodeKind::TupleField { base, index } => {
                write!(out, "tuple_field base=#{} index={index}", base.0)
            }
            NodeKind::Index { base, index } => {
                write!(out, "index base=#{} index=#{}", base.0, index.0)
            }
            NodeKind::Neg(value) => write!(out, "neg value=#{}", value.0),
            NodeKind::Not(value) => write!(out, "not value=#{}", value.0),
            NodeKind::Add { lhs, rhs } => write_binary(out, "add", *lhs, *rhs),
            NodeKind::Sub { lhs, rhs } => write_binary(out, "sub", *lhs, *rhs),
            NodeKind::Mul { lhs, rhs } => write_binary(out, "mul", *lhs, *rhs),
            NodeKind::Div { lhs, rhs } => write_binary(out, "div", *lhs, *rhs),
            NodeKind::Rem { lhs, rhs } => write_binary(out, "rem", *lhs, *rhs),
            NodeKind::Eq { lhs, rhs } => write_binary(out, "eq", *lhs, *rhs),
            NodeKind::Ne { lhs, rhs } => write_binary(out, "ne", *lhs, *rhs),
            NodeKind::Lt { lhs, rhs } => write_binary(out, "lt", *lhs, *rhs),
            NodeKind::Le { lhs, rhs } => write_binary(out, "le", *lhs, *rhs),
            NodeKind::Gt { lhs, rhs } => write_binary(out, "gt", *lhs, *rhs),
            NodeKind::Ge { lhs, rhs } => write_binary(out, "ge", *lhs, *rhs),
            NodeKind::And { lhs, rhs } => write_binary(out, "and", *lhs, *rhs),
            NodeKind::Or { lhs, rhs } => write_binary(out, "or", *lhs, *rhs),
            NodeKind::Coalesce { lhs, rhs } => write_binary(out, "coalesce", *lhs, *rhs),
            NodeKind::CallDirect { target, first_arg } => {
                write!(out, "call_direct target=fn#{} ", target.0)?;
                match self.program.function(*target) {
                    Some(callee) => {
                        out.write_str("args=")?;
                        write_ir_range(
                            out,
                            function,
                            ChildRange::new(*first_arg, callee.params.len() as u32),
                        )
                    }
                    None => write!(out, "first_arg={first_arg}"),
                }
            }
            NodeKind::CallMethod {
                target,
                first_child,
            } => {
                write!(out, "call_method target=fn#{} receiver=", target.0)?;
                write_child_at(out, function, *first_child)?;
                out.write_str(" args=")?;
                let count = self
                    .program
                    .function(*target)
                    .map_or(0, |callee| callee.params.len().saturating_sub(1) as u32);
                write_ir_range(
                    out,
                    function,
                    ChildRange::new(first_child.saturating_add(1), count),
                )
            }
            NodeKind::CallValue { first_child, argc } => {
                out.write_str("call_value callee=")?;
                write_child_at(out, function, *first_child)?;
                out.write_str(" args=")?;
                write_ir_range(
                    out,
                    function,
                    ChildRange::new(first_child.saturating_add(1), *argc),
                )
            }
            NodeKind::CallBuiltinFn { builtin, first_arg } => {
                write!(
                    out,
                    "call_builtin_fn builtin=#{} first_arg={first_arg}",
                    builtin.0
                )
            }
            NodeKind::CallBuiltinMethod {
                method,
                first_child,
            } => {
                write!(out, "call_builtin_method method=#{} receiver=", method.0)?;
                write_child_at(out, function, *first_child)?;
                write!(out, " first_arg={}", first_child.saturating_add(1))
            }
            NodeKind::CallBuiltinModule { item, first_arg } => {
                write!(
                    out,
                    "call_builtin_module item=#{} first_arg={first_arg}",
                    item.0
                )
            }
            NodeKind::CallHost { host, first_arg } => {
                write!(out, "call_host host=host#{} ", host.0)?;
                match self.program.host(*host) {
                    Some(info) => {
                        out.write_str("args=")?;
                        write_ir_range(
                            out,
                            function,
                            ChildRange::new(*first_arg, info.params.len() as u32),
                        )
                    }
                    None => write!(out, "first_arg={first_arg}"),
                }
            }
            NodeKind::Struct { type_id, layout } => {
                write!(out, "struct type=type#{} layout=#{}", type_id.0, layout.0)?;
                match function.struct_layout(*layout) {
                    Some(layout) => {
                        out.write_str(" fields=")?;
                        write_u32_list(out, &layout.fields)?;
                        out.write_str(" children=")?;
                        write_ir_range(
                            out,
                            function,
                            ChildRange::new(layout.first_child, layout.fields.len() as u32),
                        )
                    }
                    None => Ok(()),
                }
            }
            NodeKind::StructUpdate { type_id, layout } => {
                write!(
                    out,
                    "struct_update type=type#{} layout=#{}",
                    type_id.0, layout.0
                )?;
                match function.struct_layout(*layout) {
                    Some(layout) => {
                        out.write_str(" fields=")?;
                        write_u32_list(out, &layout.fields)
                    }
                    None => Ok(()),
                }
            }
            NodeKind::EnumVariant {
                variant,
                first_payload,
            } => write!(
                out,
                "enum_variant variant={variant} first_payload={first_payload}"
            ),
            NodeKind::PreludeVariant {
                variant,
                first_payload,
            } => {
                write!(
                    out,
                    "prelude_variant variant={} payloads=",
                    prelude_name(*variant)
                )?;
                let count = match variant {
                    PreludeVariant::None => 0,
                    PreludeVariant::Some | PreludeVariant::Ok | PreludeVariant::Err => 1,
                };
                write_ir_range(out, function, ChildRange::new(*first_payload, count))
            }
            NodeKind::Tuple(range) => {
                out.write_str("tuple items=")?;
                write_ir_range(out, function, *range)
            }
            NodeKind::List(range) => {
                out.write_str("list items=")?;
                write_ir_range(out, function, *range)
            }
            NodeKind::Map(range) => {
                out.write_str("map entries=")?;
                write_ir_range(out, function, *range)
            }
            NodeKind::Range { start, end } => {
                write!(out, "range start=#{} end=#{}", start.0, end.0)
            }
            NodeKind::InclusiveRange { start, end } => {
                write!(out, "inclusive_range start=#{} end=#{}", start.0, end.0)
            }
            NodeKind::Block(range) => {
                out.write_str("block stmts=")?;
                write_ir_range(out, function, *range)
            }
            NodeKind::ExprBlock(range) => {
                out.write_str("expr_block stmts=")?;
                write_ir_range(out, function, *range)
            }
            NodeKind::If(range) => {
                let children = function.child_slice(*range);
                match children {
                    [condition] => write!(out, "if condition=#{}", condition.0),
                    [condition, then] => {
                        write!(out, "if condition=#{} then=#{} else=-", condition.0, then.0)
                    }
                    [condition, then, otherwise] => write!(
                        out,
                        "if condition=#{} then=#{} else=#{}",
                        condition.0, then.0, otherwise.0
                    ),
                    _ => {
                        out.write_str("if children=")?;
                        write_ir_ids(out, children)
                    }
                }
            }
            NodeKind::While { cond, body } => {
                write!(out, "while condition=#{} body=#{}", cond.0, body.0)
            }
            NodeKind::For {
                pattern,
                first_child,
            } => {
                write!(out, "for pattern=#{} iter=", pattern.0)?;
                write_child_at(out, function, *first_child)?;
                out.write_str(" body=")?;
                write_child_at(out, function, first_child.saturating_add(1))
            }
            NodeKind::Break => out.write_str("break"),
            NodeKind::Continue => out.write_str("continue"),
            NodeKind::Return(value) => match value {
                Some(value) => write!(out, "return value=#{}", value.0),
                None => out.write_str("return value=-"),
            },
            NodeKind::Match {
                match_set,
                scrutinee,
            } => write!(
                out,
                "match match_set=#{} scrutinee=#{}",
                match_set.0, scrutinee.0
            ),
            NodeKind::Try(value) => write!(out, "try value=#{}", value.0),
            NodeKind::Let { pattern, value } => {
                write!(out, "let pattern=#{} value=#{}", pattern.0, value.0)
            }
            NodeKind::Assign { place, value } => {
                write!(out, "assign place=#{} value=#{}", place.0, value.0)
            }
            NodeKind::AddAssign { place, value } => {
                write!(out, "add_assign place=#{} value=#{}", place.0, value.0)
            }
            NodeKind::SubAssign { place, value } => {
                write!(out, "sub_assign place=#{} value=#{}", place.0, value.0)
            }
            NodeKind::MulAssign { place, value } => {
                write!(out, "mul_assign place=#{} value=#{}", place.0, value.0)
            }
            NodeKind::DivAssign { place, value } => {
                write!(out, "div_assign place=#{} value=#{}", place.0, value.0)
            }
            NodeKind::RemAssign { place, value } => {
                write!(out, "rem_assign place=#{} value=#{}", place.0, value.0)
            }
            NodeKind::ExprStmt(value) => write!(out, "expr_stmt value=#{}", value.0),
            NodeKind::ConvertInt(value) => write!(out, "convert_int value=#{}", value.0),
            NodeKind::ConvertFloat(value) => write!(out, "convert_float value=#{}", value.0),
            NodeKind::ConvertStr(value) => write!(out, "convert_str value=#{}", value.0),
            NodeKind::MakeClosure(id) => write!(out, "make_closure fn=fn#{}", id.0),
            NodeKind::StrInterp(range) => {
                out.write_str("str_interp parts=")?;
                write_str_part_range(out, self.program, function, *range)
            }
            NodeKind::Assert(range) => {
                out.write_str("assert args=")?;
                write_ir_range(out, function, *range)
            }
            NodeKind::Panic(range) => {
                out.write_str("panic args=")?;
                write_ir_range(out, function, *range)
            }
            NodeKind::MutArg(place) => write!(out, "mut_arg place=#{}", place.0),
            NodeKind::Charge(units) => write!(out, "charge units={units}"),
        }
    }

    fn write_const_ref(&self, out: &mut impl Write, id: ConstId) -> fmt::Result {
        write!(out, "const=#{}", id.0)?;
        match self.program.constant(id) {
            Some(value) => {
                out.write_str(" value=")?;
                self.write_brief_const(out, value)
            }
            None => Ok(()),
        }
    }

    fn write_brief_const(&self, out: &mut impl Write, value: &ConstValue) -> fmt::Result {
        match value {
            ConstValue::Int(value) => write!(out, "{value}"),
            ConstValue::Float(value) => write!(out, "{value:?}"),
            ConstValue::Bool(value) => write!(out, "{value}"),
            ConstValue::Str(id) => write_str_id(out, self.program, *id),
            _ => self.write_const(out, value),
        }
    }

    fn write_const(&self, out: &mut impl Write, value: &ConstValue) -> fmt::Result {
        match value {
            ConstValue::Unit => out.write_str("unit"),
            ConstValue::Int(value) => write!(out, "int {value}"),
            ConstValue::Float(value) => write!(out, "float {value:?}"),
            ConstValue::Bool(value) => write!(out, "bool {value}"),
            ConstValue::Str(id) => write_str_id(out, self.program, *id),
            ConstValue::Tuple(values) => {
                out.write_str("tuple ")?;
                self.write_const_list(out, values)
            }
            ConstValue::List(values) => {
                out.write_str("list ")?;
                self.write_const_list(out, values)
            }
            ConstValue::Map(entries) => {
                out.write_str("map {")?;
                for (index, (key, value)) in entries.iter().enumerate() {
                    if index != 0 {
                        out.write_str(", ")?;
                    }
                    self.write_const(out, key)?;
                    out.write_str(" => ")?;
                    self.write_const(out, value)?;
                }
                out.write_char('}')
            }
            ConstValue::Struct(type_id, values) => {
                write!(out, "struct type#{} ", type_id.0)?;
                self.write_const_list(out, values)
            }
            ConstValue::Enum(type_id, variant, values) => {
                write!(out, "enum type#{} variant={variant} ", type_id.0)?;
                self.write_const_list(out, values)
            }
            ConstValue::Prelude(variant, values) => {
                write!(out, "prelude {} ", prelude_name(*variant))?;
                self.write_const_list(out, values)
            }
        }
    }

    fn write_const_list(&self, out: &mut impl Write, values: &[ConstValue]) -> fmt::Result {
        out.write_char('[')?;
        for (index, value) in values.iter().enumerate() {
            if index != 0 {
                out.write_str(", ")?;
            }
            self.write_const(out, value)?;
        }
        out.write_char(']')
    }

    fn write_pattern(&self, out: &mut impl Write, pattern: &Pattern) -> fmt::Result {
        match pattern {
            Pattern::Wild => out.write_str("wild"),
            Pattern::Bind { slot, ty } => write!(out, "bind(slot=#{} ty=ty#{})", slot.0, ty.0),
            Pattern::Int(value) => write!(out, "int {value}"),
            Pattern::Bool(value) => write!(out, "bool {value}"),
            Pattern::Str(id) => write_str_id(out, self.program, *id),
            Pattern::Tuple(patterns) => {
                out.write_str("tuple(")?;
                self.write_pattern_list(out, patterns)?;
                out.write_char(')')
            }
            Pattern::Variant {
                type_id,
                variant,
                args,
            } => {
                write!(
                    out,
                    "variant(type=type#{} variant={variant} args=",
                    type_id.0
                )?;
                self.write_pattern_list(out, args)?;
                out.write_char(')')
            }
            Pattern::Prelude { variant, args } => {
                write!(out, "prelude({} args=", prelude_name(*variant))?;
                self.write_pattern_list(out, args)?;
                out.write_char(')')
            }
            Pattern::Struct { type_id, fields } => {
                write!(out, "struct(type=type#{} fields=[", type_id.0)?;
                for (index, (field, pattern)) in fields.iter().enumerate() {
                    if index != 0 {
                        out.write_str(", ")?;
                    }
                    write!(out, "{field} => ")?;
                    self.write_pattern(out, pattern)?;
                }
                out.write_str("])")
            }
            Pattern::Or(patterns) => {
                out.write_str("or(")?;
                self.write_pattern_list(out, patterns)?;
                out.write_char(')')
            }
        }
    }

    fn write_pattern_list(&self, out: &mut impl Write, patterns: &[Pattern]) -> fmt::Result {
        out.write_char('[')?;
        for (index, pattern) in patterns.iter().enumerate() {
            if index != 0 {
                out.write_str(", ")?;
            }
            self.write_pattern(out, pattern)?;
        }
        out.write_char(']')
    }
}

impl IrProgram {
    /// The complete deterministic dump; see [`Printer`].
    pub fn print(&self) -> String {
        Printer::new(self).program()
    }
}

impl fmt::Display for IrProgram {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Printer::new(self).write_program(f)
    }
}

fn write_binary(out: &mut impl Write, name: &str, lhs: IrId, rhs: IrId) -> fmt::Result {
    write!(out, "{name} lhs=#{} rhs=#{}", lhs.0, rhs.0)
}

fn write_fn_kind(out: &mut impl Write, kind: FnKind) -> fmt::Result {
    match kind {
        FnKind::Free => out.write_str("free"),
        FnKind::Method(type_id) => write!(out, "method(type#{})", type_id.0),
        FnKind::Handler => out.write_str("handler"),
        FnKind::Test => out.write_str("test"),
        FnKind::Hook => out.write_str("hook"),
        FnKind::Lambda => out.write_str("lambda"),
        FnKind::Host(host) => write!(out, "host(host#{})", host.0),
    }
}

fn write_hook(out: &mut impl Write, hook: &HookSig) -> fmt::Result {
    out.write_str("hook(name=")?;
    write_quoted(out, &hook.name)?;
    out.write_str(" module=")?;
    write_quoted(out, &hook.module)?;
    out.write_str(" params=")?;
    write_tys(out, &hook.params)?;
    write!(out, " ret=ty#{} span=", hook.ret.0)?;
    write_span(out, hook.source, hook.span)
}

fn write_tys(out: &mut impl Write, tys: &[TyId]) -> fmt::Result {
    out.write_char('[')?;
    for (index, ty) in tys.iter().enumerate() {
        if index != 0 {
            out.write_str(", ")?;
        }
        write!(out, "ty#{}", ty.0)?;
    }
    out.write_char(']')
}

fn write_u32_list(out: &mut impl Write, values: &[u32]) -> fmt::Result {
    out.write_char('[')?;
    for (index, value) in values.iter().enumerate() {
        if index != 0 {
            out.write_str(", ")?;
        }
        write!(out, "{value}")?;
    }
    out.write_char(']')
}

fn write_fn_list(out: &mut impl Write, functions: &[FnId]) -> fmt::Result {
    out.write_char('[')?;
    for (index, function) in functions.iter().enumerate() {
        if index != 0 {
            out.write_str(", ")?;
        }
        write!(out, "fn#{}", function.0)?;
    }
    out.write_char(']')
}

fn write_ir_ids(out: &mut impl Write, ids: &[IrId]) -> fmt::Result {
    out.write_char('[')?;
    for (index, id) in ids.iter().enumerate() {
        if index != 0 {
            out.write_str(", ")?;
        }
        write!(out, "#{}", id.0)?;
    }
    out.write_char(']')
}

fn write_ir_range(out: &mut impl Write, function: &IrFn, range: ChildRange) -> fmt::Result {
    match function
        .children
        .get(range.start as usize..range.end() as usize)
    {
        Some(ids) => write_ir_ids(out, ids),
        None => write!(out, "<out-of-range {}..{}>", range.start, range.end()),
    }
}

fn write_str_part_range(
    out: &mut impl Write,
    program: &IrProgram,
    function: &IrFn,
    range: ChildRange,
) -> fmt::Result {
    match function
        .str_parts
        .get(range.start as usize..range.end() as usize)
    {
        Some(parts) => {
            out.write_char('[')?;
            for (index, part) in parts.iter().enumerate() {
                if index != 0 {
                    out.write_str(", ")?;
                }
                match part {
                    StrPart::Text(id) => {
                        out.write_str("text ")?;
                        write_str_id(out, program, *id)?;
                    }
                    StrPart::Expr(id) => write!(out, "expr #{}", id.0)?,
                }
            }
            out.write_char(']')
        }
        None => write!(out, "<out-of-range {}..{}>", range.start, range.end()),
    }
}

fn write_str_id(out: &mut impl Write, program: &IrProgram, id: StrId) -> fmt::Result {
    write!(out, "str#{}", id.0)?;
    match program.string(id) {
        Some(text) => {
            out.write_char('(')?;
            write_quoted(out, text)?;
            out.write_char(')')
        }
        None => Ok(()),
    }
}

fn write_child_at(out: &mut impl Write, function: &IrFn, index: u32) -> fmt::Result {
    match function.children.get(index as usize) {
        Some(id) => write!(out, "#{}", id.0),
        None => out.write_str("?"),
    }
}

fn write_opt_fn(out: &mut impl Write, name: &str, function: Option<FnId>) -> fmt::Result {
    match function {
        Some(function) => writeln!(out, "  {name}=fn#{}", function.0),
        None => writeln!(out, "  {name}=-"),
    }
}

fn write_opt_ty(out: &mut impl Write, name: &str, ty: Option<TyId>) -> fmt::Result {
    match ty {
        Some(ty) => writeln!(out, "  {name}=ty#{}", ty.0),
        None => writeln!(out, "  {name}=-"),
    }
}

fn write_opt_type(out: &mut impl Write, name: &str, type_id: Option<TypeId>) -> fmt::Result {
    match type_id {
        Some(type_id) => writeln!(out, "  {name}=type#{}", type_id.0),
        None => writeln!(out, "  {name}=-"),
    }
}

fn write_span(out: &mut impl Write, source: SourceId, span: Span) -> fmt::Result {
    write!(out, "src#{}:{}..{}", source.0, span.start, span.end)
}

fn write_effects(out: &mut impl Write, effects: crate::Effects) -> fmt::Result {
    let mut first = true;
    for name in effects.names() {
        if !first {
            out.write_char('|')?;
        }
        out.write_str(name)?;
        first = false;
    }
    if first {
        out.write_str("NONE")?;
    }
    Ok(())
}

fn write_quoted(out: &mut impl Write, text: &str) -> fmt::Result {
    out.write_char('"')?;
    write!(out, "{}", text.escape_debug())?;
    out.write_char('"')
}

fn access_mode(mode: AccessMode) -> &'static str {
    match mode {
        AccessMode::Share => "share",
        AccessMode::Take => "take",
    }
}

fn prelude_name(variant: PreludeVariant) -> &'static str {
    match variant {
        PreludeVariant::None => "none",
        PreludeVariant::Some => "some",
        PreludeVariant::Ok => "ok",
        PreludeVariant::Err => "err",
    }
}

fn entry_kind_name(kind: crate::program::EntryKind) -> &'static str {
    match kind {
        crate::program::EntryKind::Library => "library",
        crate::program::EntryKind::Script => "script",
        crate::program::EntryKind::Plugin => "plugin",
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::charge::Charge;
    use crate::effects::Effects;
    use crate::node::Node;
    use crate::program::EntryKind;

    #[test]
    fn quoted_text_escapes_quotes_backslashes_and_controls() {
        let mut out = String::new();
        write_quoted(&mut out, "a\"b\\c\nd").expect("write quoted text");
        assert_eq!(out, "\"a\\\"b\\\\c\\nd\"");
    }

    #[test]
    fn effects_use_the_defined_flag_order() {
        let mut out = String::new();
        write_effects(
            &mut out,
            Effects::SHARES | Effects::MAY_TRAP | Effects::DYN_COST,
        )
        .expect("write effects");
        assert_eq!(out, "MAY_TRAP|SHARES|DYN_COST");

        out.clear();
        write_effects(&mut out, Effects::NONE).expect("write effects");
        assert_eq!(out, "NONE");
    }

    #[test]
    fn invalid_ids_and_missing_side_data_print_placeholders() {
        let function = IrFn {
            id: FnId::new(0),
            name: "main".into(),
            source: SourceId(7),
            kind: FnKind::Free,
            params: Vec::new(),
            ret: TyId::new(0),
            locals: 0,
            slot_info: Vec::new(),
            captures: Vec::new(),
            patterns: Vec::new(),
            matches: Vec::new(),
            struct_layouts: Vec::new(),
            str_parts: Vec::new(),
            nodes: vec![Node::new(
                NodeKind::SlotRead {
                    slot: crate::Slot::new(99),
                    mode: AccessMode::Share,
                },
                TyId::new(3),
                Charge::ZERO,
                Effects::NONE,
            )],
            spans: Vec::new(),
            synthetic: Vec::new(),
            safepoints: Vec::new(),
            children: Vec::new(),
            body: Some(IrId::new(0)),
        };
        let program = IrProgram {
            fns: vec![function],
            entries: crate::Entries {
                entry_kind: EntryKind::Library,
                ..crate::Entries::default()
            },
            ..IrProgram::default()
        };

        let dump = program.print();
        assert!(dump.contains("sources:\n  (none)"));
        assert!(dump.contains("source=src#7"));
        assert!(dump.contains("slot=#99"));
        assert!(dump.contains("ty=ty#3"));
        assert!(dump.contains("span=-"));
        assert!(dump.contains("origin=-"));
    }
}
