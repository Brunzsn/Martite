//! Embedding API and plugin host for Martite.

mod snapshot;

use std::collections::HashMap;
use std::rc::Rc;

use martite_base::{FileSystemProvider, LoadLimits, MemoryProvider, SourceProvider};
use martite_check::{
    CheckedProgram, FnSig, Ty, TypeDefKind as CheckTypeDefKind, TypeId as CheckTypeId,
};
use martite_config::{Manifest, parse_manifest};
use martite_driver::{LoadOptions, LoadedProgram, LowerOptions, load, lower_with};
use martite_ir::{HostId, IrProgram, TypeDefKind, TypeId, TypeTable};
use martite_resolve::InterfaceSource;
use martite_runtime::{
    ExecLimits, HostCaller, HostContext, HostFault, IrHostDispatcher, IrRuntime, Trap, Value,
};
use martite_syntax::ParseOptions;
use snapshot::{SNAPSHOT_FORMAT_VERSION, ValueCodec};

pub use martite_base::{Code, Diagnostic, SourceId, SourceMap, Span};
pub use martite_ir::VerifyMode;
pub use martite_runtime::{ExecLimits as Limits, Value as HostValue, show};

/// The language edition implemented by this crate.
pub const LANG_EDITION: &str = "1";

/// The host interface version implemented by this crate. Interfaces carry no
/// version comments; the embedding owns the version.
const HOST_API_VERSION: (u32, u32) = (1, 0);

/// A failed plugin load together with the sources used to compile it.
///
/// [`Engine::load`] keeps its diagnostics-only signature for compatibility.
/// Hosts that render load failures should call [`Engine::try_load`] and pass
/// [`LoadError::sources`] to [`Diagnostic::render`] or their own source
/// renderer.
#[derive(Debug, Clone)]
pub struct LoadError {
    pub diagnostics: Vec<Diagnostic>,
    pub sources: SourceMap,
}

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
            .map(|d| {
                &d.name == name && d.is_interface && matches!(&d.kind, CheckTypeDefKind::Opaque)
            })
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
    optimize: bool,
    require_host_impls: bool,
    verification: VerifyMode,
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
            max_limits: ExecLimits::default(),
            default_limits: ExecLimits::default(),
            optimize: true,
            require_host_impls: true,
            verification: VerifyMode::Always,
        }
    }

    pub fn set_limits(&mut self, max: ExecLimits, default: ExecLimits) {
        self.max_limits = max;
        self.default_limits = default;
    }

    /// Allow interface functions with no registered Rust implementation.
    ///
    /// By default every interface function must have an implementation or the
    /// load fails with E509. Test hosts opt out so a plugin can be loaded
    /// without a test double for every host function; calling an unregistered
    /// host function from a test then traps with `no test double`.
    pub fn set_allow_missing_host_impls(&mut self, allow: bool) -> &mut Self {
        self.require_host_impls = !allow;
        self
    }

    pub fn set_load_limits(&mut self, limits: LoadLimits) {
        self.load_limits = limits;
    }

    /// Enable or disable the default `Preserve` optimizer.
    ///
    /// Enabled by default. The optimizer only removes nodes while carrying
    /// their exact fuel charge, so observable behavior is unchanged.
    pub fn set_optimize(&mut self, enabled: bool) -> &mut Self {
        self.optimize = enabled;
        self
    }

    /// The IR verification policy used by loading and optimization.
    ///
    /// Defaults to [`VerifyMode::Always`], including release builds.
    pub fn verification(&self) -> VerifyMode {
        self.verification
    }

    /// Select the IR verification policy for loading and optimization.
    ///
    /// [`VerifyMode::Always`] is the default. [`VerifyMode::Never`] is for
    /// benchmarks and fuzzing only; it does not change observable behavior.
    /// [`VerifyMode::DebugOnly`] restores the older debug-build-only policy.
    pub fn set_verification(&mut self, mode: VerifyMode) -> &mut Self {
        self.verification = mode;
        self
    }

    fn optimize_ir(&self, ir: &mut IrProgram) -> Result<(), Vec<Diagnostic>> {
        if !self.optimize {
            return Ok(());
        }
        martite_opt::optimize(
            ir,
            martite_opt::OptimizeOptions {
                verify: self.verification,
                ..Default::default()
            },
        )
        .map(|_| ())
        .map_err(|error| {
            vec![Diagnostic::error(
                Code::E508,
                format!("optimization failed: {error}"),
            )]
        })
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
    ///
    /// This is the diagnostics-only variant of [`Engine::try_reload`].
    pub fn reload(
        &self,
        manifest_text: &str,
        provider: &dyn SourceProvider,
        previous: &Plugin,
        snapshot: &Snapshot,
    ) -> Result<Reload, Vec<Diagnostic>> {
        self.try_reload(manifest_text, provider, previous, snapshot)
            .map_err(|error| error.diagnostics)
    }

    /// Re-check a plugin and keep the load sources when it fails.
    pub fn try_reload(
        &self,
        manifest_text: &str,
        provider: &dyn SourceProvider,
        previous: &Plugin,
        snapshot: &Snapshot,
    ) -> Result<Reload, LoadError> {
        let plugin = self.try_load(manifest_text, provider)?;
        let fingerprint = plugin.type_fingerprint();
        let keep_snapshot = fingerprint != [0u8; 32]
            && fingerprint == previous.type_fingerprint()
            && fingerprint == snapshot.type_fingerprint;
        Ok(Reload {
            plugin,
            snapshot_kept: keep_snapshot,
            snapshot: keep_snapshot.then(|| snapshot.clone()),
        })
    }

    /// Load a plugin, keeping only the diagnostics on failure.
    ///
    /// Use [`Engine::try_load`] when the host needs the [`SourceMap`] to render
    /// the failure.
    pub fn load(
        &self,
        manifest_text: &str,
        provider: &dyn SourceProvider,
    ) -> Result<Plugin, Vec<Diagnostic>> {
        self.try_load(manifest_text, provider)
            .map_err(|error| error.diagnostics)
    }

    /// Load a plugin and keep the sources used to compile it when loading fails.
    pub fn try_load(
        &self,
        manifest_text: &str,
        provider: &dyn SourceProvider,
    ) -> Result<Plugin, LoadError> {
        let manifest = parse_manifest(manifest_text).map_err(|e| LoadError {
            diagnostics: vec![Diagnostic::error(Code::E508, e.to_string())],
            sources: SourceMap::default(),
        })?;
        self.try_load_with_manifest(manifest, provider)
    }

    /// Load a parsed manifest, keeping only the diagnostics on failure.
    ///
    /// Use [`Engine::try_load_with_manifest`] when the host needs the
    /// [`SourceMap`] to render the failure.
    pub fn load_with_manifest(
        &self,
        manifest: Manifest,
        provider: &dyn SourceProvider,
    ) -> Result<Plugin, Vec<Diagnostic>> {
        self.try_load_with_manifest(manifest, provider)
            .map_err(|error| error.diagnostics)
    }

    /// Load a parsed manifest and keep the sources used to compile it when
    /// loading fails.
    pub fn try_load_with_manifest(
        &self,
        manifest: Manifest,
        provider: &dyn SourceProvider,
    ) -> Result<Plugin, LoadError> {
        // Check the manifest's API version against the host's interface version.
        let (host_major, host_minor) = HOST_API_VERSION;
        let mut diagnostics = Vec::new();
        let (want_major, want_minor) = match parse_api(&manifest.api) {
            Ok(version) => version,
            Err(()) => {
                return Err(LoadError {
                    diagnostics: vec![Diagnostic::error(
                        Code::E508,
                        format!("malformed plugin API version `{}`", manifest.api),
                    )],
                    sources: SourceMap::default(),
                });
            }
        };
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
                format!("plugin requires newer API minor {want_minor}, host provides {host_minor}"),
            ));
        }
        if !diagnostics.is_empty() {
            return Err(LoadError {
                diagnostics,
                sources: SourceMap::default(),
            });
        }

        let loaded = load(
            provider,
            &LoadOptions {
                entry: manifest.entry.clone(),
                interfaces: self.interfaces.clone(),
                grants: manifest.grants.clone(),
                limits: self.load_limits,
                edition: manifest.lang.clone(),
                parse: ParseOptions::default(),
            },
        );
        if loaded.has_errors() {
            return Err(LoadError {
                diagnostics: loaded.diagnostics,
                sources: loaded.sources,
            });
        }
        let LoadedProgram {
            program, sources, ..
        } = loaded;
        let Some(program) = program else {
            return Err(LoadError {
                diagnostics: vec![Diagnostic::error(Code::E508, "program failed to load")],
                sources,
            });
        };
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
                        if sig.params.iter().any(|param| param.mutable) {
                            diagnostics.push(Diagnostic::error(
                                Code::E509,
                                format!(
                                    "host function `{qualified}` may not take `mut` parameters"
                                ),
                            ));
                        }
                        if !want.matches(sig, &program) {
                            diagnostics.push(Diagnostic::error(
                                Code::E509,
                                format!(
                                    "host function `{qualified}` has a mismatched Rust signature"
                                ),
                            ));
                        }
                    }
                }
            }
        }
        for name in missing {
            if self.require_host_impls {
                diagnostics.push(Diagnostic::error(
                    Code::E509,
                    format!("host function `{name}` has no Rust implementation"),
                ));
            }
        }
        if !diagnostics.is_empty() {
            return Err(LoadError {
                diagnostics,
                sources,
            });
        }
        let lowered = lower_with(
            &program,
            &sources,
            LowerOptions {
                verify: self.verification,
            },
        );
        if lowered.has_errors() {
            return Err(LoadError {
                diagnostics: lowered.diagnostics,
                sources,
            });
        }
        let Some(mut ir) = lowered.program else {
            return Err(LoadError {
                diagnostics: vec![Diagnostic::error(Code::E508, "program failed to lower")],
                sources,
            });
        };
        if let Err(diagnostics) = self.optimize_ir(&mut ir) {
            return Err(LoadError {
                diagnostics,
                sources,
            });
        }
        let host_by_id = self.host_map(&ir);
        let limits = clamp_limits(
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
        );
        let caller = HostCaller::new(manifest.name.clone(), manifest.version.clone());
        Ok(Plugin::from_inner(PluginInner {
            program,
            ir,
            sources,
            host_by_id,
            settings: manifest.settings.clone(),
            limits,
            max_limits: self.max_limits,
            caller,
            manifest,
            verification: self.verification,
        }))
    }

    /// Map the IR host table onto the registered host functions.
    ///
    /// The IR identifies hosts by a dense [`HostId`]; embedders resolve their
    /// name-based registry once, at load time.
    fn host_map(&self, ir: &IrProgram) -> Vec<Option<RegisteredFn>> {
        ir.hosts
            .iter()
            .map(|info| {
                let qualified = format!("{}.{}", info.module, info.name);
                self.host_fns.get(&qualified).cloned()
            })
            .collect()
    }

    /// Load a script, keeping only the diagnostics on failure.
    ///
    /// Use [`Engine::try_load_script`] when the host needs the [`SourceMap`] to
    /// render the failure.
    pub fn load_script(
        &self,
        entry: impl Into<String>,
        provider: &dyn SourceProvider,
    ) -> Result<Plugin, Vec<Diagnostic>> {
        self.try_load_script(entry, provider)
            .map_err(|error| error.diagnostics)
    }

    /// Load a script and keep the sources used to compile it when loading fails.
    pub fn try_load_script(
        &self,
        entry: impl Into<String>,
        provider: &dyn SourceProvider,
    ) -> Result<Plugin, LoadError> {
        let loaded = load(
            provider,
            &LoadOptions {
                entry: entry.into(),
                interfaces: self.interfaces.clone(),
                grants: self.interfaces.iter().map(|i| i.path.clone()).collect(),
                limits: self.load_limits,
                edition: LANG_EDITION.into(),
                parse: ParseOptions::default(),
            },
        );
        if loaded.has_errors() {
            return Err(LoadError {
                diagnostics: loaded.diagnostics,
                sources: loaded.sources,
            });
        }
        let LoadedProgram {
            program, sources, ..
        } = loaded;
        let Some(program) = program else {
            return Err(LoadError {
                diagnostics: vec![Diagnostic::error(Code::E508, "program failed to load")],
                sources,
            });
        };
        let mut diagnostics = Vec::new();
        for module in &program.resolved.modules {
            if !module.is_interface {
                continue;
            }
            for item in &module.ast.items {
                if let martite_syntax::Item::Fn(f) = item {
                    let qualified = format!("{}.{}", module.path, f.name);
                    if let Some(def) = program.resolved.node_to_def.get(&f.id).copied()
                        && let Some(registered) = self.host_fns.get(&qualified)
                        && let (Some(want), Some(sig)) =
                            (&registered.signature, program.fn_sig(def))
                    {
                        if sig.params.iter().any(|param| param.mutable) {
                            diagnostics.push(Diagnostic::error(
                                Code::E509,
                                format!(
                                    "host function `{qualified}` may not take `mut` parameters"
                                ),
                            ));
                        }
                        if !want.matches(sig, &program) {
                            diagnostics.push(Diagnostic::error(
                                Code::E509,
                                format!(
                                    "host function `{qualified}` has a mismatched Rust signature"
                                ),
                            ));
                        }
                    }
                }
            }
        }
        if !diagnostics.is_empty() {
            return Err(LoadError {
                diagnostics,
                sources,
            });
        }
        let lowered = lower_with(
            &program,
            &sources,
            LowerOptions {
                verify: self.verification,
            },
        );
        if lowered.has_errors() {
            return Err(LoadError {
                diagnostics: lowered.diagnostics,
                sources,
            });
        }
        let Some(mut ir) = lowered.program else {
            return Err(LoadError {
                diagnostics: vec![Diagnostic::error(Code::E508, "program failed to lower")],
                sources,
            });
        };
        if let Err(diagnostics) = self.optimize_ir(&mut ir) {
            return Err(LoadError {
                diagnostics,
                sources,
            });
        }
        let host_by_id = self.host_map(&ir);
        let manifest = Manifest {
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
        };
        let caller = HostCaller::new(manifest.name.clone(), manifest.version.clone());
        Ok(Plugin::from_inner(PluginInner {
            program,
            ir,
            sources,
            host_by_id,
            settings: toml::Value::Table(Default::default()),
            limits: clamp_limits(self.default_limits, self.max_limits),
            max_limits: self.max_limits,
            caller,
            manifest,
            verification: self.verification,
        }))
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

fn parse_api(api: &str) -> Result<(u32, u32), ()> {
    let mut parts = api.split('.');
    let major = parts.next().ok_or(())?.parse().map_err(|_| ())?;
    let minor = parts.next().ok_or(())?.parse().map_err(|_| ())?;
    if parts.next().is_some() {
        return Err(());
    }
    Ok((major, minor))
}

struct PluginInner {
    program: CheckedProgram,
    ir: IrProgram,
    sources: SourceMap,
    host_by_id: Vec<Option<RegisteredFn>>,
    settings: toml::Value,
    limits: ExecLimits,
    max_limits: ExecLimits,
    caller: HostCaller,
    manifest: Manifest,
    verification: VerifyMode,
}

impl PluginInner {
    fn has_handler(&self, name: &str) -> bool {
        self.ir
            .entries
            .handlers
            .iter()
            .any(|(handler, _)| handler.as_ref() == name)
    }
}

/// A loaded, immutable plugin program.
///
/// `Plugin` is a cheap handle to shared program data. Instances own a clone of
/// the handle, so a host may store a plugin and its instances together or drop
/// the original load handle while instances remain alive.
#[derive(Clone)]
pub struct Plugin {
    inner: Rc<PluginInner>,
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
    pub fn instantiate(&self) -> Result<Instance, Trap> {
        self.instantiate_with_limits(self.plugin.limits())
    }

    /// Instantiate with a per-call budget if `init` must run.
    ///
    /// When the snapshot is kept, this only restores it and the limits are
    /// unused.
    pub fn instantiate_with_limits(&self, limits: ExecLimits) -> Result<Instance, Trap> {
        match &self.snapshot {
            Some(snapshot) => self.plugin.instantiate_restored(snapshot),
            None => self.plugin.instantiate_with_limits(limits),
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
    fn from_inner(inner: PluginInner) -> Self {
        Self {
            inner: Rc::new(inner),
        }
    }

    pub fn program(&self) -> &CheckedProgram {
        &self.inner.program
    }

    /// The lowered, self-contained program executed by the IR evaluator.
    pub fn ir(&self) -> &IrProgram {
        &self.inner.ir
    }

    /// Re-lower the checked program without running optimizer passes.
    ///
    /// Used by the CLI's `--dump-ir=before` stage; normal execution uses
    /// [`Self::ir`].
    pub fn lowered_ir(&self) -> Option<IrProgram> {
        lower_with(
            &self.inner.program,
            &self.inner.sources,
            LowerOptions {
                verify: self.inner.verification,
            },
        )
        .program
    }

    /// IR-owned type definitions for this program.
    ///
    /// Values, `show`, JSON conversion and snapshot validation use this table;
    /// the checker table stays available for tooling through [`Self::program`].
    pub fn types(&self) -> &TypeTable {
        &self.inner.ir.types
    }

    pub fn sources(&self) -> &SourceMap {
        &self.inner.sources
    }

    pub fn manifest(&self) -> &Manifest {
        &self.inner.manifest
    }

    /// Execution limits resolved from the manifest and engine maximum.
    ///
    /// These are the defaults used by [`Self::instantiate`] and
    /// [`Instance::call`]. The `*_with_limits` variants override them for one
    /// invocation; the engine maximum still clamps the request.
    pub fn limits(&self) -> ExecLimits {
        self.inner.limits
    }

    /// Arguments passed to `init`, including the settings JSON value when the
    /// hook declares a parameter.
    pub fn init_args(&self) -> Vec<Value> {
        let Some(init) = self.inner.ir.entries.init else {
            return Vec::new();
        };
        match self.inner.ir.function(init) {
            Some(function) if function.params.is_empty() => Vec::new(),
            _ => vec![self.settings_json()],
        }
    }

    /// Create an opaque handle value for a host type declared in an interface.
    ///
    /// This is for handles created outside a plugin call, such as test
    /// fixtures. A host function that creates a handle during a call uses
    /// `HostContext::make_handle`, which derives the type from the function's
    /// declared return type.
    pub fn make_handle(&self, type_name: &str, token: impl Into<String>) -> Option<Value> {
        let type_id =
            self.types().defs.iter().position(|d| {
                d.name.as_ref() == type_name && matches!(d.kind, TypeDefKind::Opaque)
            })?;
        Some(Value::Handle(std::rc::Rc::new(
            martite_runtime::HandleValue {
                type_id: TypeId::new(type_id as u32),
                token: token.into(),
            },
        )))
    }

    /// Fingerprint of the state type, used to decide whether a snapshot can
    /// survive a reload.
    pub fn type_fingerprint(&self) -> [u8; 32] {
        self.inner
            .program
            .state_type
            .as_ref()
            .map(|ty| fingerprint_type(ty, &self.inner.program))
            .unwrap_or([0u8; 32])
    }

    /// Run `fn main` in a script, using the registered host functions.
    pub fn run_main(&self) -> Result<martite_runtime::MainResult, Trap> {
        let mut dispatcher = IrDispatch {
            fns: &self.inner.host_by_id,
            missing: MISSING_HOST,
        };
        let mut runtime = self.runtime(self.inner.limits, &mut dispatcher);
        runtime.run_main()
    }

    /// Instantiate by restoring a snapshot instead of running `init`.
    pub fn instantiate_restored(&self, snapshot: &Snapshot) -> Result<Instance, Trap> {
        let mut instance = Instance {
            plugin: self.clone(),
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
        for (def, function) in self
            .inner
            .program
            .tests
            .iter()
            .zip(&self.inner.ir.entries.tests)
        {
            let name = self.inner.program.resolved.def(*def).name.clone();
            let mut dispatcher = IrDispatch {
                fns: &self.inner.host_by_id,
                missing: MISSING_TEST_DOUBLE,
            };
            let mut runtime = self.runtime(self.inner.limits, &mut dispatcher);
            let outcome = runtime
                .invoke(*function, Vec::new())
                .map(|result| result.value);
            reports.push(TestReport {
                name,
                outcome,
                log: runtime.take_log(),
            });
        }
        reports
    }

    pub fn instantiate(&self) -> Result<Instance, Trap> {
        self.instantiate_with_limits(self.limits())
    }

    /// Instantiate with a per-call budget for `init`.
    ///
    /// The request applies to the `init` run only and is clamped by the engine
    /// maximum. Subsequent [`Instance::call`] invocations use [`Self::limits`]
    /// unless they use [`Instance::call_with_limits`].
    pub fn instantiate_with_limits(&self, limits: ExecLimits) -> Result<Instance, Trap> {
        let state = if let Some(init) = self.inner.ir.entries.init {
            let args = self.init_args();
            let mut dispatcher = IrDispatch {
                fns: &self.inner.host_by_id,
                missing: MISSING_HOST,
            };
            let mut runtime = self.runtime(limits, &mut dispatcher);
            Some(runtime.invoke(init, args)?.value)
        } else {
            None
        };
        Ok(Instance {
            plugin: self.clone(),
            state,
        })
    }

    /// Create a runtime for one plugin invocation.
    ///
    /// This is the single place where caller identity and the limit ceiling are
    /// applied, so every execution path reports the same identity and no path
    /// can exceed the engine maximum.
    fn runtime<'a, 'h>(
        &'a self,
        limits: ExecLimits,
        dispatcher: &'h mut IrDispatch<'a>,
    ) -> IrRuntime<'a, 'h> {
        let mut runtime = IrRuntime::with_host(
            &self.inner.ir,
            clamp_limits(limits, self.inner.max_limits),
            dispatcher,
        );
        runtime.set_caller(self.inner.caller.clone());
        runtime
    }

    fn settings_json(&self) -> Value {
        let json_type = self.inner.ir.entries.json_type.unwrap_or(TypeId(0));
        toml_to_json(&self.inner.settings, json_type, self.types())
    }
}

/// A live plugin instance: its state plus an owned handle to its plugin.
///
/// Instances do not borrow the [`Plugin`] that created them, so hosts can keep
/// plugins and instances in the same registry, drop the original load handle,
/// or swap a reloaded plugin into a slot without self-referential types.
pub struct Instance {
    plugin: Plugin,
    state: Option<Value>,
}

#[derive(Debug, Clone)]
pub enum Outcome {
    Handled(Value),
    NoHandler,
}

impl Instance {
    /// The plugin program this instance was created from.
    pub fn plugin(&self) -> &Plugin {
        &self.plugin
    }

    pub fn state(&self) -> Option<&Value> {
        self.state.as_ref()
    }

    pub fn call(&mut self, hook: &str, args: Vec<Value>) -> Result<Outcome, Trap> {
        self.call_with_limits(hook, args, self.plugin.limits())
    }

    /// Run one hook with a per-call limit override.
    ///
    /// The request is clamped by the engine maximum. A trap leaves the instance
    /// state unchanged.
    pub fn call_with_limits(
        &mut self,
        hook: &str,
        args: Vec<Value>,
        limits: ExecLimits,
    ) -> Result<Outcome, Trap> {
        if !self.plugin.inner.has_handler(hook) {
            return Ok(Outcome::NoHandler);
        }
        let mut dispatcher = IrDispatch {
            fns: &self.plugin.inner.host_by_id,
            missing: MISSING_HOST,
        };
        let mut runtime = self.plugin.runtime(limits, &mut dispatcher);
        let value = runtime.invoke_handler(hook, &mut self.state, args)?;
        Ok(value.map(Outcome::Handled).unwrap_or(Outcome::NoHandler))
    }

    pub fn shutdown(&mut self) -> Result<(), Trap> {
        self.shutdown_with_limits(self.plugin.limits())
    }

    /// Run the `shutdown` hook with a per-call limit override.
    pub fn shutdown_with_limits(&mut self, limits: ExecLimits) -> Result<(), Trap> {
        if self.plugin.inner.ir.entries.shutdown.is_none() {
            return Ok(());
        }
        let mut dispatcher = IrDispatch {
            fns: &self.plugin.inner.host_by_id,
            missing: MISSING_HOST,
        };
        let mut runtime = self.plugin.runtime(limits, &mut dispatcher);
        let _ = runtime.invoke_shutdown(&mut self.state)?;
        Ok(())
    }

    /// A deterministic snapshot of the state and a fingerprint of its type.
    pub fn snapshot(&self) -> Snapshot {
        let type_fingerprint = self.plugin.type_fingerprint();
        let bytes = self
            .state
            .as_ref()
            .map(|state| ValueCodec::new(self.plugin.types()).encode(state))
            .unwrap_or_default();
        Snapshot {
            format_version: SNAPSHOT_FORMAT_VERSION,
            type_fingerprint,
            bytes,
        }
    }

    /// Restore a snapshot if its type fingerprint matches this plugin.
    pub fn restore(&mut self, snapshot: &Snapshot) -> Result<(), String> {
        if snapshot.format_version != SNAPSHOT_FORMAT_VERSION {
            return Err("unsupported snapshot format version".into());
        }
        if snapshot.type_fingerprint != self.plugin.type_fingerprint() {
            return Err("snapshot type fingerprint mismatch".into());
        }
        if snapshot.bytes.is_empty() {
            self.state = None;
            return Ok(());
        }
        let value = ValueCodec::new(self.plugin.types()).decode(&snapshot.bytes)?;
        self.state = Some(value);
        Ok(())
    }
}

/// A snapshot of an instance's state plus a fingerprint of its type.
///
/// The `bytes` layout is internal and session-scoped, not a stable interchange
/// format. The language is pre-0.1.0, so the map-key schema was unified with
/// the value tags without bumping [`Snapshot::format_version`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Wire-format version. Snapshots from another version are rejected.
    pub format_version: u32,
    /// BLAKE3 fingerprint of the state type.
    pub type_fingerprint: [u8; 32],
    /// Encoded state; empty for a stateless instance.
    pub bytes: Vec<u8>,
}

const MISSING_HOST: &str = "host function is not registered";
const MISSING_TEST_DOUBLE: &str = "no test double";

struct IrDispatch<'a> {
    fns: &'a [Option<RegisteredFn>],
    missing: &'static str,
}

impl IrHostDispatcher for IrDispatch<'_> {
    fn call(
        &mut self,
        host: HostId,
        args: &[Value],
        cx: &mut HostContext,
    ) -> Result<Value, HostFault> {
        let registered = self
            .fns
            .get(host.index())
            .and_then(Option::as_ref)
            .ok_or_else(|| HostFault::new(self.missing))?;
        (registered.function)(cx, args)
    }
}

fn toml_to_json(value: &toml::Value, json_type: TypeId, types: &TypeTable) -> Value {
    let variant = |name: &str| types.variant_index(json_type, name).unwrap_or(0);
    let make = |variant: usize, payload: Vec<Value>| {
        Value::Enum(Rc::new(martite_runtime::EnumValue::new(
            json_type, variant, payload,
        )))
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
                    .map(|v| toml_to_json(v, json_type, types))
                    .collect(),
            )],
        ),
        toml::Value::Table(table) => {
            let mut map = martite_runtime::OrderedMap::new();
            for (key, value) in table {
                map.insert(
                    martite_runtime::KeyValue::Str(key.clone()),
                    toml_to_json(value, json_type, types),
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

fn fingerprint_type(ty: &Ty, program: &CheckedProgram) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"martite-state-v1");
    fingerprint_into(&mut hash, ty, program, &mut Vec::new());
    *hash.finalize().as_bytes()
}

fn hash_bytes(hash: &mut blake3::Hasher, bytes: &[u8]) {
    hash.update(&(bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

fn fingerprint_into(
    hash: &mut blake3::Hasher,
    ty: &Ty,
    program: &CheckedProgram,
    stack: &mut Vec<CheckTypeId>,
) {
    match ty {
        Ty::Int => {
            hash.update(b"int");
        }
        Ty::Float => {
            hash.update(b"float");
        }
        Ty::Bool => {
            hash.update(b"bool");
        }
        Ty::Str => {
            hash.update(b"str");
        }
        Ty::Unit => {
            hash.update(b"unit");
        }
        Ty::Range => {
            hash.update(b"range");
        }
        Ty::Never => {
            hash.update(b"never");
        }
        Ty::Error => {
            hash.update(b"error");
        }
        Ty::Tuple(items) => {
            hash.update(b"tuple");
            hash.update(&(items.len() as u64).to_le_bytes());
            for item in items {
                fingerprint_into(hash, item, program, stack);
            }
        }
        Ty::List(t) => {
            hash.update(b"list");
            fingerprint_into(hash, t, program, stack);
        }
        Ty::Map(k, v) | Ty::Result(k, v) => {
            hash.update(if matches!(ty, Ty::Map(_, _)) {
                b"map"
            } else {
                b"result"
            });
            fingerprint_into(hash, k, program, stack);
            fingerprint_into(hash, v, program, stack);
        }
        Ty::Option(t) => {
            hash.update(b"option");
            fingerprint_into(hash, t, program, stack);
        }
        Ty::Fn(params, ret) => {
            hash.update(b"fn");
            hash.update(&(params.len() as u64).to_le_bytes());
            for p in params {
                fingerprint_into(hash, p, program, stack);
            }
            fingerprint_into(hash, ret, program, stack);
        }
        Ty::Named(id, args) => {
            hash.update(b"named");
            if stack.contains(id) {
                hash.update(b"recursive");
                return;
            }
            stack.push(*id);
            if let Some(def) = program.type_def(*id) {
                hash_bytes(hash, def.name.as_bytes());
                match &def.kind {
                    CheckTypeDefKind::Opaque => {
                        hash.update(b"opaque");
                    }
                    CheckTypeDefKind::Struct { fields } => {
                        hash.update(b"struct");
                        hash.update(&(fields.len() as u64).to_le_bytes());
                        for field in fields {
                            hash_bytes(hash, field.name.as_bytes());
                            fingerprint_into(hash, &field.ty, program, stack);
                        }
                    }
                    CheckTypeDefKind::Enum { variants } => {
                        hash.update(b"enum");
                        hash.update(&(variants.len() as u64).to_le_bytes());
                        for variant in variants {
                            hash_bytes(hash, variant.name.as_bytes());
                            hash.update(&(variant.payload.len() as u64).to_le_bytes());
                            for payload in &variant.payload {
                                fingerprint_into(hash, payload, program, stack);
                            }
                        }
                    }
                }
            } else {
                hash.update(b"unknown");
            }
            stack.pop();
            hash.update(&(args.len() as u64).to_le_bytes());
            for arg in args {
                fingerprint_into(hash, arg, program, stack);
            }
        }
        Ty::Param(i) => {
            hash.update(b"param");
            hash.update(&i.to_le_bytes());
        }
        Ty::Var(i) => {
            hash.update(b"var");
            hash.update(&i.to_le_bytes());
        }
    }
}

/// Convenience for tests and examples: a sandboxed provider rooted at a directory.
pub fn fs_provider(root: impl Into<std::path::PathBuf>) -> FileSystemProvider {
    FileSystemProvider::new(root)
}

/// Convenience for the standalone CLI host or an embedding that explicitly
/// wants ambient filesystem authority.
pub fn fs_provider_unrestricted(root: impl Into<std::path::PathBuf>) -> FileSystemProvider {
    FileSystemProvider::unrestricted(root)
}

/// Convenience for in-memory embeddings.
pub fn memory_provider(files: &[(&str, &str)]) -> MemoryProvider {
    let mut provider = MemoryProvider::new();
    for (path, text) in files {
        provider.insert(*path, *text);
    }
    provider
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_versions_are_strict() {
        assert_eq!(parse_api("1.0"), Ok((1, 0)));
        assert!(parse_api("1").is_err());
        assert!(parse_api("1.0.0").is_err());
        assert!(parse_api("1.x").is_err());
        assert!(parse_api("1.0junk").is_err());
    }
}
