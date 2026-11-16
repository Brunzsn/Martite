//! Workspace discovery for the CLI and tooling.
//!
//! A workspace is a `martite.toml` with a `[workspace]` section. Members are
//! plugin project directories; interface packages are directories loaded by
//! [`InterfacePackage`]. Paths are relative to the workspace root.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use martite_config::{WorkspaceConfig, parse_tool_config};
use martite_resolve::InterfaceSource;

use crate::interfaces::{InterfacePackage, InterfacePackageError};

#[derive(Debug, Clone)]
pub struct Workspace {
    pub root: PathBuf,
    pub members: Vec<PathBuf>,
    pub interface_packages: Vec<InterfacePackage>,
    pub interfaces: Vec<InterfaceSource>,
}

#[derive(Debug, Clone)]
pub struct WorkspaceError {
    pub message: String,
}

impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for WorkspaceError {}

/// Find the nearest ancestor `martite.toml` with a `[workspace]` section and
/// load its members and interface packages.
///
/// Configs without `[workspace]` are skipped, so running from inside a member
/// that has its own tool-only `martite.toml` still finds the workspace root.
pub fn discover_workspace(start: &Path) -> Result<Option<Workspace>, WorkspaceError> {
    let Some((root, config)) = find_workspace_config(start)? else {
        return Ok(None);
    };
    build_workspace(root, config).map(Some)
}

/// Find the workspace root without loading members or interface packages.
///
/// Tools such as the formatter only need the directory to walk.
pub fn discover_workspace_root(start: &Path) -> Result<Option<PathBuf>, WorkspaceError> {
    Ok(find_workspace_config(start)?.map(|(root, _)| root))
}

fn find_workspace_config(
    start: &Path,
) -> Result<Option<(PathBuf, WorkspaceConfig)>, WorkspaceError> {
    let start = if start.is_file() {
        start.parent().unwrap_or(Path::new("."))
    } else {
        start
    };

    for directory in start.ancestors() {
        let path = directory.join("martite.toml");
        if !path.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|error| workspace_error(format!("cannot read {}: {error}", path.display())))?;
        let config = parse_tool_config(&text)
            .map_err(|error| workspace_error(format!("{}: {}", path.display(), error.message)))?;
        let Some(workspace) = config.workspace else {
            continue;
        };
        return Ok(Some((directory.to_path_buf(), workspace)));
    }
    Ok(None)
}

fn build_workspace(root: PathBuf, config: WorkspaceConfig) -> Result<Workspace, WorkspaceError> {
    let members = expand_many(&root, &config.members, "workspace member")?;
    for member in &members {
        if !member.is_dir() {
            return Err(workspace_error(format!(
                "workspace member is not a directory: {}",
                member.display()
            )));
        }
        if !member.join("plugin.toml").is_file() {
            return Err(workspace_error(format!(
                "workspace member {} has no plugin.toml",
                member.display()
            )));
        }
    }

    let package_dirs = expand_many(&root, &config.interface_packages, "interface package")?;
    let mut interface_packages = Vec::with_capacity(package_dirs.len());
    let mut package_names = HashSet::new();
    let mut module_names = HashSet::new();
    let mut interfaces = Vec::new();
    for directory in package_dirs {
        let package = InterfacePackage::load(&directory)
            .map_err(|error| interface_error(directory, error))?;
        if !package_names.insert(package.name.clone()) {
            return Err(workspace_error(format!(
                "interface package `{}` is declared more than once",
                package.name
            )));
        }
        for module in &package.modules {
            if !module_names.insert(module.path.clone()) {
                return Err(workspace_error(format!(
                    "interface module `{}` is defined by more than one package",
                    module.path
                )));
            }
        }
        interfaces.extend(package.modules.iter().cloned());
        interface_packages.push(package);
    }
    interfaces.sort_by(|left, right| left.path.cmp(&right.path));

    Ok(Workspace {
        root,
        members,
        interface_packages,
        interfaces,
    })
}

fn interface_error(directory: PathBuf, error: InterfacePackageError) -> WorkspaceError {
    workspace_error(format!("{}: {}", directory.display(), error.message))
}

/// Expand workspace path patterns. `*` matches within a segment, `**` matches
/// any number of segments, and `?` matches one character. Patterns must be
/// relative to the workspace root and may not contain `..`.
fn expand_many(
    root: &Path,
    patterns: &[String],
    label: &str,
) -> Result<Vec<PathBuf>, WorkspaceError> {
    let mut out = Vec::new();
    for pattern in patterns {
        let mut matched = Vec::new();
        expand_pattern(root, pattern, &mut matched)?;
        if matched.is_empty() {
            return Err(workspace_error(format!("no {label} matched `{pattern}`")));
        }
        out.extend(matched);
    }

    let mut seen = HashSet::new();
    out.retain(|path| {
        let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
        seen.insert(key)
    });
    out.sort();
    Ok(out)
}

fn expand_pattern(
    root: &Path,
    pattern: &str,
    out: &mut Vec<PathBuf>,
) -> Result<(), WorkspaceError> {
    let normalized = pattern.replace('\\', "/");
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized.contains(':')
        || normalized.split('/').any(|segment| segment.is_empty())
    {
        return Err(workspace_error(format!(
            "workspace path `{pattern}` must be a non-empty relative path"
        )));
    }
    let components: Vec<String> = normalized.split('/').map(str::to_string).collect();
    if components.iter().any(|component| component == "..") {
        return Err(workspace_error(format!(
            "workspace path `{pattern}` may not contain `..`"
        )));
    }

    expand_components(root, &components, out)
}

fn expand_components(
    base: &Path,
    components: &[String],
    out: &mut Vec<PathBuf>,
) -> Result<(), WorkspaceError> {
    let Some((first, rest)) = components.split_first() else {
        if base.exists() {
            out.push(base.to_path_buf());
        }
        return Ok(());
    };

    if first == "**" {
        expand_components(base, rest, out)?;
        let entries = read_dir(base)?;
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                expand_components(&path, components, out)?;
            }
        }
        return Ok(());
    }

    if first.contains('*') || first.contains('?') {
        for entry in read_dir(base)? {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !wildcard_match(first, name) {
                continue;
            }
            let path = entry.path();
            if rest.is_empty() {
                out.push(path);
            } else if path.is_dir() {
                expand_components(&path, rest, out)?;
            }
        }
        return Ok(());
    }

    let path = base.join(first);
    expand_components(&path, rest, out)
}

fn read_dir(path: &Path) -> Result<Vec<std::fs::DirEntry>, WorkspaceError> {
    let entries = std::fs::read_dir(path)
        .map_err(|error| workspace_error(format!("cannot read {}: {error}", path.display())))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| workspace_error(format!("cannot read {}: {error}", path.display())))?;
    Ok(entries)
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let mut table = vec![vec![false; text.len() + 1]; pattern.len() + 1];
    table[0][0] = true;
    for i in 0..pattern.len() {
        if pattern[i] == '*' {
            table[i + 1][0] = table[i][0];
        }
        for j in 0..text.len() {
            table[i + 1][j + 1] = match pattern[i] {
                '*' => table[i][j + 1] || table[i + 1][j],
                '?' => table[i][j],
                other => table[i][j] && other == text[j],
            };
        }
    }
    table[pattern.len()][text.len()]
}

fn workspace_error(message: impl Into<String>) -> WorkspaceError {
    WorkspaceError {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("martite-workspace-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn plugin(root: &Path, name: &str) {
        let directory = root.join("plugins").join(name);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("plugin.toml"),
            format!(
                "[plugin]\nname = \"{name}\"\nversion = \"0.1.0\"\nlang = \"1\"\napi = \"1.0\"\nentry = \"main.mt\"\n"
            ),
        )
        .unwrap();
        std::fs::write(directory.join("main.mt"), "on query(text: str) -> ()\n").unwrap();
    }

    fn interface(root: &Path) {
        let directory = root.join("interfaces").join("nvlaunch");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("interface.toml"),
            "[interface]\nname = \"nvlaunch\"\n",
        )
        .unwrap();
        std::fs::write(
            directory.join("launcher.mti"),
            "on query(text: str) -> ()\n",
        )
        .unwrap();
    }

    fn config(root: &Path, text: &str) {
        std::fs::write(root.join("martite.toml"), text).unwrap();
    }

    #[test]
    fn discovers_members_and_interfaces_from_member_directory() {
        let root = temp("discover");
        plugin(&root, "calculator");
        plugin(&root, "github");
        interface(&root);
        config(
            &root,
            "[workspace]\nmembers = [\"plugins/*\"]\ninterface-packages = [\"interfaces/nvlaunch\"]\n",
        );

        let workspace = discover_workspace(&root.join("plugins/calculator"))
            .unwrap()
            .unwrap();
        assert_eq!(workspace.root, root);
        assert_eq!(workspace.members.len(), 2);
        assert_eq!(workspace.interface_packages.len(), 1);
        assert_eq!(workspace.interfaces[0].path, "nvlaunch.launcher");
    }

    #[test]
    fn skips_tool_only_configs() {
        let root = temp("skip-tool");
        plugin(&root, "calculator");
        interface(&root);
        config(
            &root,
            "[workspace]\nmembers = [\"plugins/*\"]\ninterface-packages = [\"interfaces/nvlaunch\"]\n",
        );
        std::fs::write(
            root.join("plugins/calculator/martite.toml"),
            "[lint]\npreset = \"strict\"\n",
        )
        .unwrap();

        let workspace = discover_workspace(&root.join("plugins/calculator"))
            .unwrap()
            .unwrap();
        assert_eq!(workspace.root, root);
    }

    #[test]
    fn missing_member_manifest_is_rejected() {
        let root = temp("missing-manifest");
        std::fs::create_dir_all(root.join("plugins/empty")).unwrap();
        config(&root, "[workspace]\nmembers = [\"plugins/*\"]\n");

        let error = discover_workspace(&root).unwrap_err();
        assert!(error.message.contains("no plugin.toml"));
    }

    #[test]
    fn parent_directory_is_rejected() {
        let root = temp("parent");
        config(&root, "[workspace]\nmembers = [\"../plugins/*\"]\n");
        let error = discover_workspace(&root).unwrap_err();
        assert!(error.message.contains("may not contain `..`"));
    }

    #[test]
    fn wildcard_matching() {
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("a?c", "abc"));
        assert!(!wildcard_match("a?c", "ac"));
        assert!(!wildcard_match("a*c", "ab"));
        assert!(wildcard_match("a*c", "abbbc"));
    }

    #[test]
    fn root_discovery_does_not_load_packages() {
        let root = temp("root-only");
        plugin(&root, "calculator");
        std::fs::create_dir_all(root.join("interfaces/broken")).unwrap();
        config(
            &root,
            "[workspace]\nmembers = [\"plugins/*\"]\ninterface-packages = [\"interfaces/broken\"]\n",
        );

        assert_eq!(discover_workspace_root(&root).unwrap(), Some(root.clone()));
        assert!(discover_workspace(&root).is_err());
    }
}
