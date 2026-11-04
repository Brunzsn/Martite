//! Embedding API and plugin host for Martite.

use std::collections::HashMap;
use std::rc::Rc;

use martite_base::{
    Code, Diagnostic, FileSystemProvider, LoadLimits, MemoryProvider, SourceMap, SourceProvider,
};
use martite_check::{CheckedProgram, FnSig, Ty, TypeDefKind, TypeId};
use martite_config::{parse_manifest, Manifest};
use martite_driver::{load, LoadOptions};
use martite_resolve::{DefId, InterfaceSource};
use martite_runtime::{
    ExecLimits, HostContext, HostDispatcher, HostFault, Runtime, Trap, Value,
};

pub use martite_base::{SourceId, Span};
pub use martite_runtime::{show, ExecLimits as Limits, Value as HostValue};

/// The language edition implemented by this crate.
pub const LANG_EDITION: &str = "1";

/// The host interface version implemented by this crate. Interfaces carry no
/// version comments; the embedding owns the version.
const HOST_API_VERSION: (u32, u32) = (1, 0);

pub type HostFn = dyn Fn(&mut HostContext, &[Value]) -> Result<Value, HostFault>;

#[derive(Debug, Clone, PartialEq)]
pub enum HostType {
    Unit,
    Int,
    Float,
    Bool,
    Str,
    List(Box<HostType>),
    Map(Box<HostType>, Box<HostType>),
    Option(Box<HostType>),
    Result(Box<HostType>, Box<HostType>),
    Handle(String),
    Any,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HostSignature {
    pub params: Vec<HostType>,
    pub ret: HostType,
}

impl HostSignature {
    pub fn new(params: Vec<HostType>, ret: HostType) -> Self {
        Self { params, ret }
    }

    fn matches(&self, sig: &FnSig, program: &CheckedProgram) -> bool {
        if self.params.len() != sig.params.len() {
            return false;
        }
        self.params
            .iter()
            .zip(sig.params.iter())
            .all(|(host, ty)| host_type_matches(host, &ty.ty, program))
            && host_type_matches(&self.ret, &sig.ret, program)
    }
}

fn host_type_matches(host: &HostType, ty: &Ty, program: &CheckedProgram) -> bool {
    if matches!(host, HostType::Any) {
        return true;
    }
    match (host, ty) {
        (HostType::Unit, Ty::Unit)
        | (HostType::Int, Ty::Int)
        | (HostType::Float, Ty::Float)
        | (HostType::Bool, Ty::Bool)
        | (HostType::Str, Ty::Str) => true,
        (HostType::List(a), Ty::List(b)) => host_type_matches(a, b, program),
        (HostType::Map(ak, av), Ty::Map(bk, bv)) => {
            host_type_matches(ak, bk, program) && host_type_matches(av, bv, program)
        }
        (HostType::Option(a), Ty::Option(b)) => host_type_matches(a, b, program),
        (HostType::Result(a, b), Ty::Result(c, d)) => {
            host_type_matches(a, c, program) && host_type_matches(b, d, program)
        }
        (HostType::Handle(name), Ty::Named(id, _)) => program
            .type_def(*id)
            .map(|d| &d.name == name)
            .unwrap_or(false),
        _ => false,
    }
}

#[derive(Clone)]
struct RegisteredFn {
    signature: Option<HostSignature>,
    function: Rc<HostFn>,
}

#[derive(Clone)]
pub struct Engine {
    interfaces: Vec<InterfaceSource>,
    host_fns: HashMap<String, RegisteredFn>,
    load_limits: LoadLimits,
    max_limits: ExecLimits,
    default_limits: ExecLimits,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    pub fn new() -> Self {
        Self {
            interfaces: Vec::new(),
            host_fns: HashMap::new(),
            load_limits: LoadLimits::default(),
            max_limits: ExecLimits {
                fuel: 1_000_000,
                memory: 8 << 20,
                depth: 200,
                deadline: None,
            },
            default_limits: ExecLimits::default(),
        }
    }

    pub fn set_limits(&mut self, max: ExecLimits, default: ExecLimits) {
        self.max_limits = max;
        self.default_limits = default;
    }

    pub fn set_load_limits(&mut self, limits: LoadLimits) {
        self.load_limits = limits;
    }

    /// Register an `.mti` interface module, for example `app.editor`.
    pub fn interface(&mut self, path: impl Into<String>, text: impl Into<String>) -> &mut Self {
        self.interfaces.push(InterfaceSource {
            path: path.into(),
            text: text.into(),
        });
        self
    }

    /// Register a host function under its fully qualified interface name.
    pub fn host_fn(
        &mut self,
        name: impl Into<String>,
        signature: HostSignature,
        function: impl Fn(&mut HostContext, &[Value]) -> Result<Value, HostFault> + 'static,
    ) -> &mut Self {
        self.host_fns.insert(
            name.into(),
            RegisteredFn {
                signature: Some(signature),
                function: Rc::new(function),
            },
        );
        self
    }

    /// Register a host function without a checked Rust-side signature.
    pub fn raw_host_fn(
        &mut self,
        name: impl Into<String>,
        function: impl Fn(&mut HostContext, &[Value]) -> Result<Value, HostFault> + 'static,
    ) -> &mut Self {
        self.host_fns.insert(
            name.into(),
            RegisteredFn {
                signature: None,
                function: Rc::new(function),
            },
        );
        self
    }

    /// Register a fake host function for an offline `test` block. This is the
    /// same mechanism as [`Engine::host_fn`], named for the test-double use case.
    pub fn test_double_fn(
        &mut self,
        name: impl Into<String>,
        signature: HostSignature,
        function: impl Fn(&mut HostContext, &[Value]) -> Result<Value, HostFault> + 'static,
    ) -> &mut Self {
        self.host_fn(name, signature, function)
    }

    /// Re-check a plugin and decide whether its previous snapshot can be kept.
    ///
    /// The returned [`Reload`] calls `init` when the state fingerprint changed,
    /// and restores the snapshot without calling `init` when it did not.
    pub fn reload(
        &self,
        manifest_text: &str,
        provider: &dyn SourceProvider,
        previous: &Plugin,
        snapshot: &Snapshot,
    ) -> Result<Reload, Vec<Diagnostic>> {
        let plugin = self.load(manifest_text, provider)?;
        let fingerprint = plugin.type_fingerprint();
        let keep_snapshot = fingerprint != 0
            && fingerprint == previous.type_fingerprint()
            && fingerprint == snapshot.type_fingerprint;
        Ok(Reload {
            plugin,
            snapshot_kept: keep_snapshot,
            snapshot: keep_snapshot.then(|| snapshot.clone()),
        })
    }

    pub fn load(
        &self,
        manifest_text: &str,
        provider: &dyn SourceProvider,
    ) -> Result<Plugin, Vec<Diagnostic>> {
        let manifest = parse_manifest(manifest_text)
            .map_err(|e| vec![Diagnostic::error(Code::E508, e.to_string())])?;
        self.load_with_manifest(manifest, provider)
    }

    pub fn load_with_manifest(
        &self,
        manifest: Manifest,
        provider: &dyn SourceProvider,
    ) -> Result<Plugin, Vec<Diagnostic>> {
        // Check the manifest's API version against the host's interface version.
        let (host_major, host_minor) = HOST_API_VERSION;
        let (want_major, want_minor) = parse_api(&manifest.api);
        let mut diagnostics = Vec::new();
        if want_major != host_major {
            diagnostics.push(Diagnostic::error(
                Code::E508,
                format!(
                    "plugin requires API {want_major}.{want_minor}, host provides {host_major}.{host_minor}"
                ),
            ));
        } else if want_minor > host_minor {
            diagnostics.push(Diagnostic::error(
                Code::E508,
                format!(
                    "plugin requires newer API minor {want_minor}, host provides {host_minor}"
                ),
            ));
        }
        if !diagnostics.is_empty() {
            return Err(diagnostics);
        }

        let loaded = load(
            provider,
            &LoadOptions {
                entry: manifest.entry.clone(),
                interfaces: self.interfaces.clone(),
                grants: manifest.grants.clone(),
                limits: self.load_limits,
                edition: manifest.lang.clone(),
            },
        );
        if loaded.has_errors() {
            return Err(loaded.diagnostics);
        }
        let program = loaded.program.ok_or_else(|| {
            vec![Diagnostic::error(Code::E508, "program failed to load")]
        })?;
        let mut host_by_def = HashMap::new();
        let mut missing = Vec::new();
        for module in &program.resolved.modules {
            if !module.is_interface {
                continue;
            }
            for item in &module.ast.items {
                if let martite_syntax::Item::Fn(f) = item {
                    let qualified = format!("{}.{}", module.path, f.name);
                    let Some(def) = program.resolved.node_to_def.get(&f.id).copied() else {
                        continue;
                    };
                    let Some(registered) = self.host_fns.get(&qualified) else {
                        missing.push(qualified);
                        continue;
                    };
                    if let (Some(want), Some(sig)) = (&registered.signature, program.fn_sig(def)) {
                        if !want.matches(sig, &program) {
                            diagnostics.push(Diagnostic::error(
                                Code::E509,
                                format!("host function `{qualified}` has a mismatched Rust signature"),
                            ));
                        }
                    }
                    host_by_def.insert(def, registered.clone());
                }
            }
        }
        for name in missing {
            diagnostics.push(Diagnostic::error(
                Code::E509,
                format!("host function `{name}` has no Rust implementation"),
            ));
        }
        if !diagnostics.is_empty() {
            return Err(diagnostics);
        }
        Ok(Plugin {
            program,
            sources: loaded.sources,
            host_by_def,
            settings: manifest.settings.clone(),
            limits: clamp_limits(
                ExecLimits {
                    fuel: manifest.fuel.unwrap_or(self.default_limits.fuel),
                    memory: manifest.memory.unwrap_or(self.default_limits.memory),
                    depth: manifest.depth.unwrap_or(self.default_limits.depth),
                    deadline: manifest
                        .deadline_ms
                        .map(std::time::Duration::from_millis)
                        .or(self.default_limits.deadline),
                },
                self.max_limits,
            ),
            manifest,
        })
    }

    pub fn load_script(
        &self,
        entry: impl Into<String>,
        provider: &dyn SourceProvider,
    ) -> Result<Plugin, Vec<Diagnostic>> {
        let loaded = load(
            provider,
            &LoadOptions {
                entry: entry.into(),
                interfaces: self.interfaces.clone(),
                grants: self
                    .interfaces
                    .iter()
                    .map(|i| i.path.clone())
                    .collect(),
                limits: self.load_limits,
                edition: LANG_EDITION.into(),
            },
        );
        if loaded.has_errors() {
            return Err(loaded.diagnostics);
        }
        let program = loaded.program.ok_or_else(|| {
            vec![Diagnostic::error(Code::E508, "program failed to load")]
        })?;
        let mut host_by_def = HashMap::new();
        for module in &program.resolved.modules {
            if !module.is_interface {
                continue;
            }
            for item in &module.ast.items {
                if let martite_syntax::Item::Fn(f) = item {
                    let qualified = format!("{}.{}", module.path, f.name);
                    if let Some(def) = program.resolved.node_to_def.get(&f.id).copied() {
                        if let Some(registered) = self.host_fns.get(&qualified) {
                            host_by_def.insert(def, registered.clone());
                        }
                    }
                }
            }
        }
        Ok(Plugin {
            program,
            sources: loaded.sources,
            host_by_def,
            settings: toml::Value::Table(Default::default()),
            limits: self.default_limits,
            manifest: Manifest {
                name: "script".into(),
                version: "0.0.0".into(),
                lang: LANG_EDITION.into(),
                api: "1.0".into(),
                entry: "main.mt".into(),
                grants: Vec::new(),
                fuel: None,
                memory: None,
                depth: None,
                deadline_ms: None,
                settings: toml::Value::Table(Default::default()),
            },
        })
    }
}

fn clamp_limits(requested: ExecLimits, max: ExecLimits) -> ExecLimits {
    ExecLimits {
        fuel: requested.fuel.min(max.fuel),
        memory: requested.memory.min(max.memory),
        depth: requested.depth.min(max.depth),
        deadline: match (requested.deadline, max.deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        },
    }
}

fn parse_api(api: &str) -> (u32, u32) {
    let mut parts = api.split('.');
    let major = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    (major, minor)
}

pub struct Plugin {
    program: CheckedProgram,
    sources: SourceMap,
    host_by_def: HashMap<DefId, RegisteredFn>,
    settings: toml::Value,
    limits: ExecLimits,
    manifest: Manifest,
}

/// The result of [`Engine::reload`].
pub struct Reload {
    pub plugin: Plugin,
    /// True when the state type fingerprint is unchanged and the snapshot was
    /// kept; `instantiate` then restores it instead of calling `init`.
    pub snapshot_kept: bool,
    snapshot: Option<Snapshot>,
}

impl Reload {
    pub fn instantiate(&self) -> Result<Instance<'_>, Trap> {
        match &self.snapshot {
            Some(snapshot) => self.plugin.instantiate_restored(snapshot),
            None => self.plugin.instantiate(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TestReport {
    pub name: String,
    pub outcome: Result<Value, Trap>,
    pub log: Vec<String>,
}

impl TestReport {
    pub fn passed(&self) -> bool {
        self.outcome.is_ok()
    }
}

impl Plugin {
    pub fn program(&self) -> &CheckedProgram {
        &self.program
    }

    pub fn sources(&self) -> &SourceMap {
        &self.sources
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Create an opaque handle value for a host type declared in an interface.
    pub fn make_handle(&self, type_name: &str, token: impl Into<String>) -> Option<Value> {
        let type_id = self
            .program
            .types
            .defs
            .iter()
            .find(|d| d.name == type_name && matches!(d.kind, TypeDefKind::Opaque))
            .map(|d| d.id)?;
        Some(Value::Handle(std::rc::Rc::new(
            martite_runtime::HandleValue {
                type_id,
                token: token.into(),
            },
        )))
    }

    /// Fingerprint of the state type, used to decide whether a snapshot can
    /// survive a reload.
    pub fn type_fingerprint(&self) -> u64 {
        self.program
            .state_type
            .as_ref()
            .map(|ty| fingerprint_type(ty, &self.program))
            .unwrap_or(0)
    }

    /// Run `fn main` in a script, using the registered host functions.
    pub fn run_main(&self) -> Result<martite_runtime::MainResult, Trap> {
        let mut dispatcher = Dispatch {
            fns: &self.host_by_def,
        };
        let mut runtime = Runtime::with_host(&self.program, self.limits, &mut dispatcher);
        runtime.run_main()
    }

    /// Instantiate by restoring a snapshot instead of running `init`.
    pub fn instantiate_restored(&self, snapshot: &Snapshot) -> Result<Instance<'_>, Trap> {
        let mut instance = Instance {
            plugin: self,
            state: None,
        };
        instance.restore(snapshot).map_err(|message| Trap::Panic {
            message,
            source: None,
            span: None,
            trace: Vec::new(),
        })?;
        Ok(instance)
    }

    /// Run every `test` block using the registered host functions as test
    /// doubles. Tests that call an unregistered host function fail with a trap.
    pub fn run_tests(&self) -> Vec<TestReport> {
        let mut reports = Vec::new();
        for def in &self.program.tests {
            let name = self.program.resolved.def(*def).name.clone();
            let mut dispatcher = Dispatch {
                fns: &self.host_by_def,
            };
            let mut runtime =
                Runtime::with_host(&self.program, self.limits, &mut dispatcher);
            let outcome = runtime.invoke(*def, Vec::new()).map(|result| result.value);
            reports.push(TestReport {
                name,
                outcome,
                log: runtime.take_log(),
            });
        }
        reports
    }

    pub fn instantiate(&self) -> Result<Instance<'_>, Trap> {
        let state = if let Some(init) = self.program.init {
            let sig = self
                .program
                .fn_sig(init)
                .ok_or_else(|| Trap::HostFault("missing init signature".into()))?;
            let args = if sig.params.is_empty() {
                Vec::new()
            } else {
                vec![self.settings_json()]
            };
            let mut dispatcher = Dispatch {
                fns: &self.host_by_def,
            };
            let mut runtime = Runtime::with_host(&self.program, self.limits, &mut dispatcher);
            let result = runtime.invoke(init, args)?;
            Some(result.value)
        } else {
            None
        };
        Ok(Instance {
            plugin: self,
            state,
        })
    }

    fn settings_json(&self) -> Value {
        let json_type = self.program.json_type.unwrap_or(TypeId(0));
        toml_to_json(&self.settings, json_type, &self.program)
    }
}

pub struct Instance<'p> {
    plugin: &'p Plugin,
    state: Option<Value>,
}

#[derive(Debug, Clone)]
pub enum Outcome {
    Handled(Value),
    NoHandler,
}

impl<'p> Instance<'p> {
    pub fn state(&self) -> Option<&Value> {
        self.state.as_ref()
    }

    pub fn call(&mut self, hook: &str, args: Vec<Value>) -> Result<Outcome, Trap> {
        if !self.plugin.program.handlers.contains_key(hook) {
            return Ok(Outcome::NoHandler);
        }
        let mut dispatcher = Dispatch {
            fns: &self.plugin.host_by_def,
        };
        let mut runtime =
            Runtime::with_host(&self.plugin.program, self.plugin.limits, &mut dispatcher);
        let value = runtime.invoke_handler(hook, &mut self.state, args)?;
        Ok(value
            .map(Outcome::Handled)
            .unwrap_or(Outcome::NoHandler))
    }

    pub fn shutdown(&mut self) -> Result<(), Trap> {
        if !self.plugin.program.handlers.contains_key("shutdown") {
            return Ok(());
        }
        let mut dispatcher = Dispatch {
            fns: &self.plugin.host_by_def,
        };
        let mut runtime =
            Runtime::with_host(&self.plugin.program, self.plugin.limits, &mut dispatcher);
        let _ = runtime.invoke_handler("shutdown", &mut self.state, Vec::new())?;
        Ok(())
    }

    /// A deterministic snapshot of the state and a fingerprint of its type.
    pub fn snapshot(&self) -> Snapshot {
        let type_fingerprint = self.plugin.type_fingerprint();
        let bytes = self
            .state
            .as_ref()
            .map(|state| encode_value(state, &self.plugin.program))
            .unwrap_or_default();
        Snapshot {
            type_fingerprint,
            bytes,
        }
    }

    /// Restore a snapshot if its type fingerprint matches this plugin.
    pub fn restore(&mut self, snapshot: &Snapshot) -> Result<(), String> {
        if snapshot.type_fingerprint != self.plugin.type_fingerprint() {
            return Err("snapshot type fingerprint mismatch".into());
        }
        if snapshot.bytes.is_empty() {
            self.state = None;
            return Ok(());
        }
        let mut pos = 0;
        let value = decode_value(&snapshot.bytes, &mut pos, &self.plugin.program)?;
        if pos != snapshot.bytes.len() {
            return Err("trailing bytes in snapshot".into());
        }
        self.state = Some(value);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub type_fingerprint: u64,
    pub bytes: Vec<u8>,
}

struct Dispatch<'a> {
    fns: &'a HashMap<DefId, RegisteredFn>,
}

impl HostDispatcher for Dispatch<'_> {
    fn call(
        &mut self,
        def: DefId,
        args: &[Value],
        cx: &mut HostContext,
    ) -> Result<Value, HostFault> {
        let function = self
            .fns
            .get(&def)
            .map(|r| r.function.clone())
            .ok_or_else(|| HostFault::new("host function is not registered"))?;
        function(cx, args)
    }
}

fn toml_to_json(value: &toml::Value, json_type: TypeId, program: &CheckedProgram) -> Value {
    let variant = |name: &str| {
        program
            .types
            .variant_index(json_type, name)
            .unwrap_or(0)
    };
    let make = |variant: usize, payload: Vec<Value>| {
        Value::Enum(Rc::new(martite_runtime::EnumValue {
            type_id: json_type,
            variant,
            payload,
        }))
    };
    match value {
        toml::Value::String(s) => make(variant("Str"), vec![Value::str(s.clone())]),
        toml::Value::Integer(i) => make(variant("Num"), vec![Value::Float(*i as f64)]),
        toml::Value::Float(f) => make(variant("Num"), vec![Value::Float(*f)]),
        toml::Value::Boolean(b) => make(variant("Bool"), vec![Value::Bool(*b)]),
        toml::Value::Array(items) => make(
            variant("Arr"),
            vec![Value::list(
                items
                    .iter()
                    .map(|v| toml_to_json(v, json_type, program))
                    .collect(),
            )],
        ),
        toml::Value::Table(table) => {
            let mut map = martite_runtime::OrderedMap::new();
            for (key, value) in table {
                map.insert(
                    martite_runtime::KeyValue::Str(key.clone()),
                    toml_to_json(value, json_type, program),
                );
            }
            make(variant("Obj"), vec![Value::map(map)])
        }
        toml::Value::Datetime(_) => make(variant("Null"), vec![]),
    }
}

// ----------------------------------------------------------------------
// Snapshots
// ----------------------------------------------------------------------

fn fingerprint_type(ty: &Ty, program: &CheckedProgram) -> u64 {
    let mut hash = Fnv::new();
    fingerprint_into(&mut hash, ty, program, &mut Vec::new());
    hash.finish()
}

fn fingerprint_into(hash: &mut Fnv, ty: &Ty, program: &CheckedProgram, stack: &mut Vec<TypeId>) {
    match ty {
        Ty::Int => hash.write(b"int"),
        Ty::Float => hash.write(b"float"),
        Ty::Bool => hash.write(b"bool"),
        Ty::Str => hash.write(b"str"),
        Ty::Unit => hash.write(b"unit"),
        Ty::Range => hash.write(b"range"),
        Ty::Never => hash.write(b"never"),
        Ty::Error => hash.write(b"error"),
        Ty::Tuple(items) => {
            hash.write(b"tuple");
            for item in items {
                fingerprint_into(hash, item, program, stack);
            }
            hash.write(b"end");
        }
        Ty::List(t) => {
            hash.write(b"list");
            fingerprint_into(hash, t, program, stack);
        }
        Ty::Map(k, v) | Ty::Result(k, v) => {
            hash.write(if matches!(ty, Ty::Map(_, _)) { b"map" } else { b"result" });
            fingerprint_into(hash, k, program, stack);
            fingerprint_into(hash, v, program, stack);
        }
        Ty::Option(t) => {
            hash.write(b"option");
            fingerprint_into(hash, t, program, stack);
        }
        Ty::Fn(params, ret) => {
            hash.write(b"fn");
            for p in params {
                fingerprint_into(hash, p, program, stack);
            }
            fingerprint_into(hash, ret, program, stack);
        }
        Ty::Named(id, args) => {
            hash.write(b"named");
            if stack.contains(id) {
                hash.write(b"recursive");
                return;
            }
            stack.push(*id);
            if let Some(def) = program.type_def(*id) {
                hash.write(def.name.as_bytes());
                match &def.kind {
                    TypeDefKind::Opaque => hash.write(b"opaque"),
                    TypeDefKind::Struct { fields } => {
                        hash.write(b"struct");
                        for field in fields {
                            hash.write(field.name.as_bytes());
                            fingerprint_into(hash, &field.ty, program, stack);
                        }
                    }
                    TypeDefKind::Enum { variants } => {
                        hash.write(b"enum");
                        for variant in variants {
                            hash.write(variant.name.as_bytes());
                            for payload in &variant.payload {
                                fingerprint_into(hash, payload, program, stack);
                            }
                        }
                    }
                }
            }
            stack.pop();
            for arg in args {
                fingerprint_into(hash, arg, program, stack);
            }
        }
        Ty::Param(i) => {
            hash.write(b"param");
            hash.write(&i.to_le_bytes());
        }
        Ty::Var(i) => {
            hash.write(b"var");
            hash.write(&i.to_le_bytes());
        }
    }
}

struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf29ce484222325)
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= *byte as u64;
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

fn encode_value(value: &Value, program: &CheckedProgram) -> Vec<u8> {
    let mut out = Vec::new();
    encode_into(&mut out, value, program);
    out
}

fn encode_into(out: &mut Vec<u8>, value: &Value, program: &CheckedProgram) {
    use Value::*;
    match value {
        Unit => out.push(0),
        Int(v) => {
            out.push(1);
            out.extend_from_slice(&v.to_le_bytes());
        }
        Float(v) => {
            out.push(2);
            out.extend_from_slice(&v.to_bits().to_le_bytes());
        }
        Bool(v) => {
            out.push(3);
            out.push(*v as u8);
        }
        Str(v) => {
            out.push(4);
            write_bytes(out, v.as_bytes());
        }
        List(items) | Tuple(items) => {
            out.push(if matches!(value, List(_)) { 5 } else { 6 });
            write_len(out, items.len());
            for item in items.iter() {
                encode_into(out, item, program);
            }
        }
        Map(map) => {
            out.push(7);
            write_len(out, map.len());
            for (key, value) in map.entries() {
                encode_key(out, key);
                encode_into(out, value, program);
            }
        }
        Range {
            start,
            end,
            inclusive,
        } => {
            out.push(8);
            out.extend_from_slice(&start.to_le_bytes());
            out.extend_from_slice(&end.to_le_bytes());
            out.push(*inclusive as u8);
        }
        Struct(value) => {
            out.push(9);
            write_len(out, value.type_id.0 as usize);
            write_len(out, value.fields.len());
            for field in &value.fields {
                encode_into(out, field, program);
            }
        }
        Enum(value) => {
            out.push(10);
            write_len(out, value.type_id.0 as usize);
            write_len(out, value.variant);
            write_len(out, value.payload.len());
            for field in &value.payload {
                encode_into(out, field, program);
            }
        }
        Option(value) => {
            out.push(11);
            match value {
                None => out.push(0),
                Some(value) => {
                    out.push(1);
                    encode_into(out, value, program);
                }
            }
        }
        Result(result) => {
            out.push(12);
            match result {
                Ok(value) => {
                    out.push(0);
                    encode_into(out, value, program);
                }
                Err(value) => {
                    out.push(1);
                    encode_into(out, value, program);
                }
            }
        }
        Handle(value) => {
            out.push(13);
            write_len(out, value.type_id.0 as usize);
            write_bytes(out, value.token.as_bytes());
        }
        Fn(_) | Builtin(_) | BuiltinModuleFn { .. } => out.push(14),
    }
}

fn write_len(out: &mut Vec<u8>, value: usize) {
    out.extend_from_slice(&(value as u64).to_le_bytes());
}

fn write_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    write_len(out, bytes.len());
    out.extend_from_slice(bytes);
}

fn encode_key(out: &mut Vec<u8>, key: &martite_runtime::KeyValue) {
    let value = martite_runtime::key_to_value(key);
    encode_scalar_key(out, &value);
}

fn encode_scalar_key(out: &mut Vec<u8>, value: &Value) {
    match value {
        Value::Int(v) => {
            out.push(0);
            out.extend_from_slice(&v.to_le_bytes());
        }
        Value::Bool(v) => {
            out.push(1);
            out.push(*v as u8);
        }
        Value::Str(v) => {
            out.push(2);
            write_bytes(out, v.as_bytes());
        }
        Value::Tuple(items) => {
            out.push(3);
            write_len(out, items.len());
            for item in items.iter() {
                encode_scalar_key(out, item);
            }
        }
        Value::Struct(value) => {
            out.push(4);
            write_len(out, value.type_id.0 as usize);
            write_len(out, value.fields.len());
            for field in &value.fields {
                encode_scalar_key(out, field);
            }
        }
        Value::Enum(value) => {
            out.push(5);
            write_len(out, value.type_id.0 as usize);
            write_len(out, value.variant);
            write_len(out, value.payload.len());
            for field in &value.payload {
                encode_scalar_key(out, field);
            }
        }
        Value::Option(value) => {
            out.push(6);
            match value {
                None => out.push(0),
                Some(value) => {
                    out.push(1);
                    encode_scalar_key(out, value);
                }
            }
        }
        Value::Result(result) => {
            out.push(7);
            match result {
                Ok(value) => {
                    out.push(0);
                    encode_scalar_key(out, value);
                }
                Err(value) => {
                    out.push(1);
                    encode_scalar_key(out, value);
                }
            }
        }
        Value::Handle(value) => {
            out.push(8);
            write_len(out, value.type_id.0 as usize);
            write_bytes(out, value.token.as_bytes());
        }
        _ => out.push(9),
    }
}

fn decode_value(bytes: &[u8], pos: &mut usize, program: &CheckedProgram) -> Result<Value, String> {
    let tag = read_u8(bytes, pos)?;
    Ok(match tag {
        0 => Value::Unit,
        1 => Value::Int(read_i64(bytes, pos)?),
        2 => Value::Float(f64::from_bits(read_u64(bytes, pos)?)),
        3 => Value::Bool(read_u8(bytes, pos)? != 0),
        4 => Value::str(read_string(bytes, pos)?),
        5 => {
            let len = read_len(bytes, pos)?;
            let mut items = Vec::with_capacity(len);
            for _ in 0..len {
                items.push(decode_value(bytes, pos, program)?);
            }
            Value::list(items)
        }
        6 => {
            let len = read_len(bytes, pos)?;
            let mut items = Vec::with_capacity(len);
            for _ in 0..len {
                items.push(decode_value(bytes, pos, program)?);
            }
            Value::tuple(items)
        }
        7 => {
            let len = read_len(bytes, pos)?;
            let mut map = martite_runtime::OrderedMap::new();
            for _ in 0..len {
                let key = decode_key(bytes, pos)?;
                let value = decode_value(bytes, pos, program)?;
                map.insert(key, value);
            }
            Value::map(map)
        }
        8 => Value::Range {
            start: read_i64(bytes, pos)?,
            end: read_i64(bytes, pos)?,
            inclusive: read_u8(bytes, pos)? != 0,
        },
        9 => {
            let type_id = TypeId(read_len(bytes, pos)? as u32);
            let len = read_len(bytes, pos)?;
            let mut fields = Vec::with_capacity(len);
            for _ in 0..len {
                fields.push(decode_value(bytes, pos, program)?);
            }
            Value::Struct(Rc::new(martite_runtime::StructValue { type_id, fields }))
        }
        10 => {
            let type_id = TypeId(read_len(bytes, pos)? as u32);
            let variant = read_len(bytes, pos)?;
            let len = read_len(bytes, pos)?;
            let mut payload = Vec::with_capacity(len);
            for _ in 0..len {
                payload.push(decode_value(bytes, pos, program)?);
            }
            Value::Enum(Rc::new(martite_runtime::EnumValue {
                type_id,
                variant,
                payload,
            }))
        }
        11 => match read_u8(bytes, pos)? {
            0 => Value::Option(None),
            _ => Value::Option(Some(Box::new(decode_value(bytes, pos, program)?))),
        },
        12 => match read_u8(bytes, pos)? {
            0 => Value::Result(Ok(Box::new(decode_value(bytes, pos, program)?))),
            _ => Value::Result(Err(Box::new(decode_value(bytes, pos, program)?))),
        },
        13 => {
            let type_id = TypeId(read_len(bytes, pos)? as u32);
            let token = read_string(bytes, pos)?;
            Value::Handle(Rc::new(martite_runtime::HandleValue { type_id, token }))
        }
        _ => return Err("snapshot contains a non-data value".into()),
    })
}

fn decode_key(bytes: &[u8], pos: &mut usize) -> Result<martite_runtime::KeyValue, String> {
    use martite_runtime::KeyValue;
    let tag = read_u8(bytes, pos)?;
    Ok(match tag {
        0 => KeyValue::Int(read_i64(bytes, pos)?),
        1 => KeyValue::Bool(read_u8(bytes, pos)? != 0),
        2 => KeyValue::Str(read_string(bytes, pos)?),
        3 => {
            let len = read_len(bytes, pos)?;
            let mut items = Vec::with_capacity(len);
            for _ in 0..len {
                items.push(decode_key(bytes, pos)?);
            }
            KeyValue::Tuple(items)
        }
        4 => {
            let type_id = TypeId(read_len(bytes, pos)? as u32);
            let len = read_len(bytes, pos)?;
            let mut fields = Vec::with_capacity(len);
            for _ in 0..len {
                fields.push(decode_key(bytes, pos)?);
            }
            KeyValue::Struct(type_id, fields)
        }
        5 => {
            let type_id = TypeId(read_len(bytes, pos)? as u32);
            let variant = read_len(bytes, pos)?;
            let len = read_len(bytes, pos)?;
            let mut payload = Vec::with_capacity(len);
            for _ in 0..len {
                payload.push(decode_key(bytes, pos)?);
            }
            KeyValue::Enum(type_id, variant, payload)
        }
        6 => {
            if read_u8(bytes, pos)? == 0 {
                KeyValue::Option(None)
            } else {
                KeyValue::Option(Some(Box::new(decode_key(bytes, pos)?)))
            }
        }
        7 => {
            if read_u8(bytes, pos)? == 0 {
                KeyValue::Result(Ok(Box::new(decode_key(bytes, pos)?)))
            } else {
                KeyValue::Result(Err(Box::new(decode_key(bytes, pos)?)))
            }
        }
        8 => {
            let type_id = TypeId(read_len(bytes, pos)? as u32);
            let token = read_string(bytes, pos)?;
            KeyValue::Handle(type_id, token)
        }
        _ => return Err("invalid map key in snapshot".into()),
    })
}

fn read_u8(bytes: &[u8], pos: &mut usize) -> Result<u8, String> {
    let byte = *bytes.get(*pos).ok_or("truncated snapshot")?;
    *pos += 1;
    Ok(byte)
}

fn read_u64(bytes: &[u8], pos: &mut usize) -> Result<u64, String> {
    let slice = bytes.get(*pos..*pos + 8).ok_or("truncated snapshot")?;
    *pos += 8;
    Ok(u64::from_le_bytes(slice.try_into().unwrap()))
}

fn read_i64(bytes: &[u8], pos: &mut usize) -> Result<i64, String> {
    Ok(read_u64(bytes, pos)? as i64)
}

fn read_len(bytes: &[u8], pos: &mut usize) -> Result<usize, String> {
    Ok(read_u64(bytes, pos)? as usize)
}

fn read_string(bytes: &[u8], pos: &mut usize) -> Result<String, String> {
    let len = read_len(bytes, pos)?;
    let slice = bytes.get(*pos..*pos + len).ok_or("truncated snapshot string")?;
    *pos += len;
    String::from_utf8(slice.to_vec()).map_err(|_| "invalid UTF-8 in snapshot".into())
}

/// Convenience for tests and examples: a provider rooted at a directory.
pub fn fs_provider(root: impl Into<std::path::PathBuf>) -> FileSystemProvider {
    FileSystemProvider::new(root)
}

/// Convenience for in-memory embeddings.
pub fn memory_provider(files: &[(&str, &str)]) -> MemoryProvider {
    let mut provider = MemoryProvider::new();
    for (path, text) in files {
        provider.insert(*path, *text);
    }
    provider
}
