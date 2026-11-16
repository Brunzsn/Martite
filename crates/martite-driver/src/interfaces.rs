//! Interface package discovery.
//!
//! An interface package is a directory of `.mti` files. The package name is
//! read from `interface.toml` when present, otherwise from the directory name.
//! Module paths are the package name followed by the file path inside the
//! package, with `/` replaced by `.`, for example `interfaces/nvlaunch/host.mti`
//! is module `nvlaunch.host`.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use martite_config::{is_valid_module_segment, parse_interface_manifest};
use martite_resolve::InterfaceSource;

#[derive(Debug, Clone)]
pub struct InterfacePackage {
    pub name: String,
    pub version: Option<String>,
    pub root: PathBuf,
    pub modules: Vec<InterfaceSource>,
}

#[derive(Debug, Clone)]
pub struct InterfacePackageError {
    pub message: String,
}

impl std::fmt::Display for InterfacePackageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for InterfacePackageError {}

impl InterfacePackage {
    /// Load and validate one interface package directory.
    pub fn load(root: &Path) -> Result<Self, InterfacePackageError> {
        if !root.is_dir() {
            return Err(fail(format!(
                "interface package directory not found: {}",
                root.display()
            )));
        }

        let manifest_path = root.join("interface.toml");
        let (name, version) = if manifest_path.is_file() {
            let text = std::fs::read_to_string(&manifest_path)
                .map_err(|e| fail(format!("cannot read {}: {e}", manifest_path.display())))?;
            let manifest = parse_interface_manifest(&text)
                .map_err(|e| fail(format!("{}: {}", manifest_path.display(), e.message)))?;
            (manifest.name, manifest.version)
        } else {
            let name = root
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    fail(format!(
                        "cannot derive an interface package name from {}",
                        root.display()
                    ))
                })?
                .to_string();
            (name, None)
        };

        if !is_valid_module_segment(&name) {
            return Err(fail(format!(
                "interface package name `{name}` must be a lowercase module segment ([a-z][a-z0-9_]*)"
            )));
        }

        let mut files = Vec::new();
        collect_files(root, root, &name, &mut files)?;
        if files.is_empty() {
            return Err(fail(format!(
                "interface package `{name}` has no .mti files in {}",
                root.display()
            )));
        }
        files.sort_by(|left, right| left.0.cmp(&right.0));

        let mut seen = HashSet::new();
        let mut modules = Vec::with_capacity(files.len());
        for (module, path) in files {
            if !seen.insert(module.clone()) {
                return Err(fail(format!(
                    "interface package `{name}` defines module `{module}` more than once"
                )));
            }
            let text = std::fs::read_to_string(&path)
                .map_err(|e| fail(format!("cannot read {}: {e}", path.display())))?;
            modules.push(InterfaceSource { path: module, text });
        }

        Ok(Self {
            name,
            version,
            root: root.to_path_buf(),
            modules,
        })
    }
}

fn collect_files(
    package_root: &Path,
    directory: &Path,
    package_name: &str,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<(), InterfacePackageError> {
    let mut entries = std::fs::read_dir(directory)
        .map_err(|e| fail(format!("cannot read {}: {e}", directory.display())))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| fail(format!("cannot read {}: {e}", directory.display())))?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| fail(format!("cannot stat {}: {e}", path.display())))?;
        if file_type.is_dir() {
            collect_files(package_root, &path, package_name, out)?;
            continue;
        }
        if !file_type.is_file()
            || path.extension().and_then(|extension| extension.to_str()) != Some("mti")
        {
            continue;
        }

        let relative = path.strip_prefix(package_root).map_err(|_| {
            fail(format!(
                "interface file {} is outside package {}",
                path.display(),
                package_root.display()
            ))
        })?;
        let mut segments = Vec::new();
        for component in relative.components() {
            let Component::Normal(segment) = component else {
                return Err(fail(format!(
                    "interface file {} has an invalid path",
                    path.display()
                )));
            };
            let segment = segment.to_str().ok_or_else(|| {
                fail(format!(
                    "interface file {} is not valid UTF-8",
                    path.display()
                ))
            })?;
            let segment = segment.strip_suffix(".mti").unwrap_or(segment);
            if !is_valid_module_segment(segment) {
                return Err(fail(format!(
                    "interface file {} has invalid module segment `{segment}`",
                    path.display()
                )));
            }
            segments.push(segment.to_string());
        }
        if segments.is_empty() {
            return Err(fail(format!(
                "interface file {} has no module name",
                path.display()
            )));
        }
        let module = format!("{package_name}.{}", segments.join("."));
        out.push((module, path));
    }
    Ok(())
}

fn fail(message: impl Into<String>) -> InterfacePackageError {
    InterfacePackageError {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("martite-interface-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let package = root.join("nvlaunch");
        std::fs::create_dir_all(&package).unwrap();
        package
    }

    #[test]
    fn loads_modules_from_directory_name() {
        let root = temp("dir-name");
        std::fs::write(root.join("launcher.mti"), "on query(text: str) -> ()\n").unwrap();
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::write(root.join("nested/host.mti"), "fn ping() -> ()\n").unwrap();

        let package = InterfacePackage::load(&root).unwrap();
        assert_eq!(package.name, "nvlaunch");
        assert_eq!(
            package
                .modules
                .iter()
                .map(|module| module.path.clone())
                .collect::<Vec<_>>(),
            vec![
                "nvlaunch.launcher".to_string(),
                "nvlaunch.nested.host".to_string()
            ]
        );
    }

    #[test]
    fn interface_manifest_supplies_name() {
        let root = temp("manifest-name");
        std::fs::write(
            root.join("interface.toml"),
            "[interface]\nname = \"other\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(root.join("launcher.mti"), "on query(text: str) -> ()\n").unwrap();

        let package = InterfacePackage::load(&root).unwrap();
        assert_eq!(package.name, "other");
        assert_eq!(package.version.as_deref(), Some("0.1.0"));
        assert_eq!(package.modules[0].path, "other.launcher");
    }

    #[test]
    fn empty_package_is_rejected() {
        let root = temp("empty");
        let error = InterfacePackage::load(&root).unwrap_err();
        assert!(error.message.contains("no .mti files"));
    }

    #[test]
    fn invalid_file_segment_is_rejected() {
        let root = temp("bad-segment");
        std::fs::write(root.join("open-plugin.mti"), "fn ping() -> ()\n").unwrap();
        let error = InterfacePackage::load(&root).unwrap_err();
        assert!(error.message.contains("invalid module segment"));
    }
}
