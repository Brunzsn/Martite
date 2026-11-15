//! Architecture checks over Cargo's resolved workspace and package graphs.
//!
//! These tests deliberately do not scan manifest or source text: comments,
//! aliases and formatting cannot trip them. Source-level vocabulary rules are
//! enforced by dependency isolation instead.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use cargo_metadata::{DependencyKind, Metadata, Package, PackageId};
use serde::Deserialize;

const CRATES: &[&str] = &[
    "martite-base",
    "martite-fuel",
    "martite-syntax",
    "martite-config",
    "martite-resolve",
    "martite-check",
    "martite-ir",
    "martite-opt",
    "martite-runtime",
    "martite-driver",
    "martite-lint",
    "martite-fmt",
    "martite",
    "martite-cli",
    "martite-conformance",
    "martite-bench",
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn workspace_metadata() -> &'static Metadata {
    static METADATA: OnceLock<Metadata> = OnceLock::new();
    METADATA.get_or_init(|| {
        let mut command = cargo_metadata::MetadataCommand::new();
        command.manifest_path(workspace_root().join("Cargo.toml"));
        command.current_dir(workspace_root());
        command.other_options(vec!["--locked".to_owned()]);
        command
            .exec()
            .expect("cargo metadata for the workspace failed")
    })
}

struct ProbeDir(PathBuf);

impl ProbeDir {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("martite-architecture-probe-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create architecture probe directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ProbeDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn toml_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Resolve a package that depends only on the runtime and the optimizer.
///
/// The workspace graph includes dev-dependencies and feature unification from
/// every member, so `cargo metadata` for the workspace cannot show whether the
/// runtime by itself enables `martite-ir/lower`. This representative build can.
fn probe_metadata() -> &'static Metadata {
    static METADATA: OnceLock<Metadata> = OnceLock::new();
    METADATA.get_or_init(|| {
        let root = workspace_root();
        let probe = ProbeDir::new();
        let manifest = format!(
            r#"[package]
name = "architecture-probe"
version = "0.0.0"
edition = "2021"
publish = false

[workspace]

[dependencies]
martite-runtime = {{ path = '{}' }}
martite-opt = {{ path = '{}' }}
"#,
            toml_path(&root.join("crates/martite-runtime")),
            toml_path(&root.join("crates/martite-opt")),
        );
        fs::write(probe.path().join("Cargo.toml"), manifest)
            .expect("write architecture probe manifest");
        fs::create_dir_all(probe.path().join("src"))
            .expect("create architecture probe source directory");
        fs::write(probe.path().join("src/lib.rs"), "").expect("write architecture probe source");

        let mut command = cargo_metadata::MetadataCommand::new();
        command.manifest_path(probe.path().join("Cargo.toml"));
        command.current_dir(probe.path());
        command.other_options(vec!["--offline".to_owned()]);
        command
            .exec()
            .expect("cargo metadata for the architecture probe failed")
    })
}

fn package<'a>(metadata: &'a Metadata, name: &str) -> &'a Package {
    metadata
        .packages
        .iter()
        .find(|package| package.name == name)
        .unwrap_or_else(|| panic!("missing package: {name}"))
}

fn workspace_packages(metadata: &Metadata) -> Vec<&Package> {
    metadata
        .workspace_members
        .iter()
        .map(|id| {
            metadata
                .packages
                .iter()
                .find(|package| &package.id == id)
                .expect("workspace member must have a package")
        })
        .collect()
}

fn workspace_crate_names(metadata: &Metadata) -> BTreeSet<&str> {
    workspace_packages(metadata)
        .into_iter()
        .map(|package| package.name.as_str())
        .collect()
}

fn resolve_node<'a>(metadata: &'a Metadata, id: &PackageId) -> &'a cargo_metadata::Node {
    metadata
        .resolve
        .as_ref()
        .expect("cargo metadata must include a resolve graph")
        .nodes
        .iter()
        .find(|node| &node.id == id)
        .unwrap_or_else(|| panic!("missing resolve node for {id}"))
}

#[test]
fn workspace_is_split_into_pipeline_crates() {
    let metadata = workspace_metadata();
    let actual = workspace_crate_names(metadata);
    let expected: BTreeSet<&str> = CRATES.iter().copied().collect();
    assert_eq!(actual, expected, "workspace members changed");
}

#[test]
fn benchmarks_are_isolated_from_production_crates() {
    let metadata = workspace_metadata();
    let bench = package(metadata, "martite-bench");
    assert!(
        bench.publish.as_ref().is_some_and(Vec::is_empty),
        "bench crate must not publish"
    );
    assert!(
        bench
            .dependencies
            .iter()
            .any(|dependency| dependency.name == "criterion")
            && bench
                .dependencies
                .iter()
                .any(|dependency| dependency.name == "stats_alloc"),
        "bench crate must own the benchmark-only tooling"
    );

    for package in workspace_packages(metadata) {
        if package.name == "martite-bench" {
            continue;
        }
        for dependency in &package.dependencies {
            assert!(
                !matches!(
                    dependency.name.as_str(),
                    "martite-bench" | "criterion" | "stats_alloc"
                ),
                "{} must not depend on benchmark-only crate {} ({})",
                package.name,
                dependency.name,
                dependency.kind
            );
        }
    }
}

#[test]
fn optimizer_stays_above_the_ir() {
    let metadata = workspace_metadata();
    let optimizer = package(metadata, "martite-opt");
    let mut ir_dependency = None;
    for dependency in optimizer
        .dependencies
        .iter()
        .filter(|dependency| dependency.kind != DependencyKind::Development)
    {
        assert!(
            !matches!(
                dependency.name.as_str(),
                "martite-runtime"
                    | "martite-check"
                    | "martite-syntax"
                    | "martite-driver"
                    | "martite-bench"
            ),
            "martite-opt production dependencies must not include {}",
            dependency.name
        );
        if dependency.name == "martite-ir" {
            ir_dependency = Some(dependency);
        }
    }

    let ir_dependency = ir_dependency.expect("martite-opt must depend on martite-ir");
    assert!(
        !ir_dependency.uses_default_features,
        "martite-opt must not enable martite-ir default features"
    );
    assert!(
        !ir_dependency
            .features
            .iter()
            .any(|feature| feature == "lower"),
        "martite-opt must not enable martite-ir/lower"
    );
}

#[test]
fn ir_stays_above_the_runtime() {
    let metadata = workspace_metadata();
    let ir = package(metadata, "martite-ir");
    for dependency in &ir.dependencies {
        assert!(
            !matches!(
                dependency.name.as_str(),
                "martite-runtime" | "martite-bench"
            ),
            "martite-ir must not depend on {}",
            dependency.name
        );
    }
}

#[test]
fn runtime_is_ir_only() {
    let metadata = workspace_metadata();
    let runtime = package(metadata, "martite-runtime");
    let workspace_names = workspace_crate_names(metadata);
    for dependency in &runtime.dependencies {
        if workspace_names.contains(dependency.name.as_str()) {
            assert!(
                matches!(
                    dependency.name.as_str(),
                    "martite-base" | "martite-fuel" | "martite-ir"
                ),
                "martite-runtime must use the IR vocabulary, not {}",
                dependency.name
            );
        }
    }

    let ir_dependency = runtime
        .dependencies
        .iter()
        .find(|dependency| dependency.name == "martite-ir")
        .expect("martite-runtime must depend on martite-ir");
    assert_eq!(ir_dependency.kind, DependencyKind::Normal);
    assert!(
        !ir_dependency.uses_default_features,
        "martite-runtime must not enable martite-ir default features"
    );
    assert!(
        !ir_dependency
            .features
            .iter()
            .any(|feature| feature == "lower"),
        "martite-runtime must not enable martite-ir/lower"
    );

    for package in workspace_packages(metadata) {
        assert!(
            !package.features.contains_key("ast-eval"),
            "{} still defines the removed ast-eval feature",
            package.name
        );
    }
    let runtime_node = resolve_node(metadata, &runtime.id);
    assert!(
        !runtime_node
            .features
            .iter()
            .any(|feature| feature == "ast-eval"),
        "the resolved runtime build must not enable ast-eval"
    );

    let source = Path::new(runtime.manifest_path.as_str())
        .parent()
        .expect("runtime manifest must have a parent directory")
        .join("src");
    assert!(
        source.join("ir_eval.rs").is_file(),
        "the IR evaluator must be built"
    );
    assert!(
        source.join("ir_eval/builtins.rs").is_file(),
        "the IR builtin include must be built"
    );
    assert!(
        !source.join("interp.rs").exists(),
        "the AST evaluator must be removed from the runtime"
    );
    assert!(
        !source.join("builtins.rs").exists(),
        "the AST builtin include must be deleted"
    );
}

#[test]
fn runtime_and_optimizer_resolve_without_checker_crates() {
    let metadata = probe_metadata();
    let allowed = [
        "martite-base",
        "martite-fuel",
        "martite-ir",
        "martite-runtime",
        "martite-opt",
    ];
    for package in &metadata.packages {
        if package.name.starts_with("martite-") {
            assert!(
                allowed.contains(&package.name.as_str()),
                "unexpected crate in the runtime/optimizer graph: {}",
                package.name
            );
        }
        assert!(
            !matches!(package.name.as_str(), "criterion" | "stats_alloc"),
            "benchmark-only tooling leaked into the runtime/optimizer graph: {}",
            package.name
        );
    }

    let ir = package(metadata, "martite-ir");
    let ir_node = resolve_node(metadata, &ir.id);
    assert!(
        !ir_node.features.iter().any(|feature| feature == "lower"),
        "the runtime/optimizer build must not enable martite-ir/lower"
    );
}

#[test]
fn ir_lowering_is_an_opt_in_dependency() {
    let manifest_text =
        fs::read_to_string(workspace_root().join("Cargo.toml")).expect("read workspace manifest");
    let manifest: WorkspaceManifest =
        toml::from_str(&manifest_text).expect("parse workspace manifest");
    let dependency: IrWorkspaceDependency = manifest
        .workspace
        .dependencies
        .get("martite-ir")
        .expect("workspace martite-ir dependency")
        .clone()
        .try_into()
        .expect("parse workspace martite-ir dependency");
    assert_eq!(dependency.path, "crates/martite-ir");
    assert_eq!(dependency.version, manifest.workspace.package.version);
    assert!(
        !dependency.default_features,
        "the workspace IR dependency must not enable default features"
    );
    assert!(
        dependency.features.is_empty(),
        "the workspace IR dependency must not enable features"
    );

    let metadata = workspace_metadata();
    let ir = package(metadata, "martite-ir");
    let lower = ir
        .features
        .get("lower")
        .expect("martite-ir must define the lower feature");
    assert_eq!(
        lower.iter().map(String::as_str).collect::<Vec<_>>(),
        ["dep:martite-check", "dep:martite-syntax"],
        "lowering must pull in the checker and parser"
    );

    let mut edges = 0;
    for package in workspace_packages(metadata) {
        for dependency in package
            .dependencies
            .iter()
            .filter(|dependency| dependency.name == "martite-ir")
        {
            edges += 1;
            assert!(
                !dependency.uses_default_features,
                "{} must not enable martite-ir default features",
                package.name
            );
        }
    }
    assert!(edges > 0, "no workspace crate depends on martite-ir");

    let ir_node = resolve_node(metadata, &ir.id);
    assert!(
        ir_node.features.iter().any(|feature| feature == "lower"),
        "a workspace build must explicitly opt into martite-ir/lower"
    );

    let probe = probe_metadata();
    let probe_ir = package(probe, "martite-ir");
    let probe_ir_node = resolve_node(probe, &probe_ir.id);
    assert!(
        !probe_ir_node
            .features
            .iter()
            .any(|feature| feature == "lower"),
        "the runtime/optimizer build must not enable martite-ir/lower"
    );
}

#[derive(Deserialize)]
struct WorkspaceManifest {
    workspace: WorkspaceSection,
}

#[derive(Deserialize)]
struct WorkspaceSection {
    package: WorkspacePackage,
    dependencies: std::collections::BTreeMap<String, toml::Value>,
}

#[derive(Deserialize)]
struct WorkspacePackage {
    version: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct IrWorkspaceDependency {
    path: String,
    version: String,
    #[serde(default = "default_true")]
    default_features: bool,
    #[serde(default)]
    features: Vec<String>,
}

fn default_true() -> bool {
    true
}
