//! Manifest and tool configuration for Martite.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PluginSection {
    pub name: Option<String>,
    pub version: Option<String>,
    pub lang: Option<String>,
    pub api: Option<String>,
    pub entry: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PermissionsSection {
    #[serde(default)]
    pub grant: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct LimitsSection {
    pub fuel: Option<u64>,
    pub memory: Option<u64>,
    pub depth: Option<u32>,
    pub deadline_ms: Option<u64>,
}

/// `[workspace]` in `martite.toml`.
///
/// Paths are relative to the directory containing `martite.toml`. Members are
/// plugin projects; interface packages are directories discovered by the same
/// loader the embedding API uses.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSection {
    #[serde(default)]
    pub members: Vec<String>,
    #[serde(default, rename = "interface-packages", alias = "interface_packages")]
    pub interface_packages: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceConfig {
    pub members: Vec<String>,
    pub interface_packages: Vec<String>,
}

/// `interface.toml` inside an interface package directory.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct InterfaceFile {
    interface: InterfaceSection,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct InterfaceSection {
    pub name: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Clone)]
pub struct InterfaceManifest {
    pub name: String,
    pub version: Option<String>,
}

/// A module segment is one component of a dotted module path, for example
/// `launcher` in `nvlaunch.launcher`.
pub fn is_valid_module_segment(segment: &str) -> bool {
    let mut chars = segment.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Whether a manifest `entry` is a safe project-relative path.
///
/// The entry is loaded as a path, not as a dotted module name, so every
/// component is checked literally. Components may not be empty, `.`, `..`, or
/// start with `.` (a leading dot would otherwise round-trip through a module
/// name as an absolute path). Windows strips trailing spaces and dots from path
/// components, so those are rejected on every platform to keep the rule
/// deterministic.
pub fn is_valid_entry_path(entry: &str) -> bool {
    if entry.is_empty() || entry.starts_with('/') || entry.starts_with('\\') || entry.contains(':')
    {
        return false;
    }
    entry.split(['/', '\\']).all(|part| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && !part.starts_with('.')
            && !part.ends_with(' ')
            && !part.ends_with('.')
            && !part.contains('\0')
    })
}

pub fn parse_interface_manifest(text: &str) -> Result<InterfaceManifest, ManifestError> {
    let file: InterfaceFile = toml::from_str(text).map_err(|e| ManifestError {
        message: format!("malformed interface.toml: {e}"),
    })?;
    let name = file.interface.name.ok_or_else(|| ManifestError {
        message: "interface.name is required".into(),
    })?;
    if !is_valid_module_segment(&name) {
        return Err(ManifestError {
            message: "interface.name must be a lowercase module segment ([a-z][a-z0-9_]*)".into(),
        });
    }
    Ok(InterfaceManifest {
        name,
        version: file.interface.version,
    })
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ManifestFile {
    pub plugin: PluginSection,
    #[serde(default)]
    pub permissions: PermissionsSection,
    #[serde(default)]
    pub limits: LimitsSection,
    pub settings: Option<toml::Value>,
}

/// A validated plugin manifest.
#[derive(Debug, Clone)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub lang: String,
    pub api: String,
    pub entry: String,
    pub grants: Vec<String>,
    pub fuel: Option<u64>,
    pub memory: Option<u64>,
    pub depth: Option<u32>,
    pub deadline_ms: Option<u64>,
    /// Validated `[settings]` table.
    ///
    /// TOML datetimes are rejected: the settings `Json` vocabulary has no
    /// datetime variant, so mapping one to `Null` would hide it from the
    /// plugin and mapping it to `Str` would invent a convention.
    pub settings: toml::Value,
}

#[derive(Debug, Clone)]
pub struct ManifestError {
    pub message: String,
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ManifestError {}

pub fn parse_manifest(text: &str) -> Result<Manifest, ManifestError> {
    let file: ManifestFile = toml::from_str(text).map_err(|e| ManifestError {
        message: format!("malformed plugin.toml: {e}"),
    })?;
    let plugin = file.plugin;
    let name = plugin.name.ok_or_else(|| ManifestError {
        message: "plugin.name is required".into(),
    })?;
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(ManifestError {
            message: "plugin.name must match [a-z0-9-]+".into(),
        });
    }
    let version = plugin.version.ok_or_else(|| ManifestError {
        message: "plugin.version is required".into(),
    })?;
    let lang = plugin.lang.ok_or_else(|| ManifestError {
        message: "plugin.lang is required".into(),
    })?;
    if lang != "1" {
        return Err(ManifestError {
            message: format!("unsupported language edition `{lang}`"),
        });
    }
    let api = plugin.api.ok_or_else(|| ManifestError {
        message: "plugin.api is required".into(),
    })?;
    let entry = plugin.entry.ok_or_else(|| ManifestError {
        message: "plugin.entry is required".into(),
    })?;
    if !is_valid_entry_path(&entry) {
        return Err(ManifestError {
            message: "plugin.entry must be a relative path inside the project".into(),
        });
    }
    let settings = file
        .settings
        .unwrap_or_else(|| toml::Value::Table(Default::default()));
    if !matches!(settings, toml::Value::Table(_)) {
        return Err(ManifestError {
            message: "settings must be a table".into(),
        });
    }
    if contains_datetime(&settings) {
        return Err(ManifestError {
            message: "settings may not contain TOML datetimes".into(),
        });
    }
    let mut grants = file.permissions.grant;
    grants.sort();
    grants.dedup();
    Ok(Manifest {
        name,
        version,
        lang,
        api,
        entry,
        grants,
        fuel: file.limits.fuel,
        memory: file.limits.memory,
        depth: file.limits.depth,
        deadline_ms: file.limits.deadline_ms,
        settings,
    })
}

/// Datetimes stay rejected because the settings `Json` vocabulary has no
/// datetime variant. The snapshot codec is unrelated: it encodes plugin state,
/// not manifest settings.
fn contains_datetime(value: &toml::Value) -> bool {
    match value {
        toml::Value::Datetime(_) => true,
        toml::Value::Array(items) => items.iter().any(contains_datetime),
        toml::Value::Table(table) => table.values().any(contains_datetime),
        _ => false,
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ToolFile {
    #[serde(default)]
    pub workspace: Option<WorkspaceSection>,
    #[serde(default)]
    pub tool: ToolSection,
    #[serde(default)]
    pub lint: LintSection,
    /// `[fmt]` is the canonical section name. `[format]` is accepted as a
    /// deprecated compatibility alias so existing configuration files keep
    /// working; both names in one file are rejected as a duplicate field.
    #[serde(default, alias = "format")]
    pub fmt: FmtSection,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ToolSection {
    pub lang: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct LintSection {
    pub preset: Option<String>,
    #[serde(default)]
    pub groups: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub severity: std::collections::HashMap<String, String>,
    #[serde(default, alias = "threshold")]
    pub thresholds: std::collections::HashMap<String, i64>,
    #[serde(default)]
    pub options: std::collections::HashMap<String, toml::Value>,
    #[serde(default, rename = "override")]
    pub overrides: Vec<LintOverride>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct LintOverride {
    #[serde(default)]
    pub paths: Vec<String>,
    pub preset: Option<String>,
    #[serde(default)]
    pub groups: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub severity: std::collections::HashMap<String, String>,
    #[serde(default, alias = "threshold")]
    pub thresholds: std::collections::HashMap<String, i64>,
    #[serde(default)]
    pub options: std::collections::HashMap<String, toml::Value>,
    #[serde(default)]
    pub off: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct FmtSection {
    pub max_width: Option<usize>,
    pub indent: Option<usize>,
    pub end_of_line: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndOfLine {
    Lf,
    Crlf,
}

impl EndOfLine {
    pub const fn as_str(self) -> &'static str {
        match self {
            EndOfLine::Lf => "lf",
            EndOfLine::Crlf => "crlf",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FmtConfig {
    pub max_width: usize,
    pub indent: usize,
    pub end_of_line: EndOfLine,
}

impl Default for FmtConfig {
    fn default() -> Self {
        Self {
            max_width: 100,
            indent: 4,
            end_of_line: EndOfLine::Lf,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ToolConfig {
    pub workspace: Option<WorkspaceConfig>,
    pub lang: Option<String>,
    pub lint_preset: Option<String>,
    pub lint_groups: std::collections::HashMap<String, String>,
    pub lint_severity: std::collections::HashMap<String, String>,
    pub lint_thresholds: std::collections::HashMap<String, i64>,
    pub lint_options: std::collections::HashMap<String, toml::Value>,
    pub lint_overrides: Vec<LintOverride>,
    pub fmt: FmtConfig,
}

pub fn parse_tool_config(text: &str) -> Result<ToolConfig, ManifestError> {
    let file: ToolFile = toml::from_str(text).map_err(|e| ManifestError {
        message: format!("malformed martite.toml: {e}"),
    })?;
    let fmt = parse_fmt_config(file.fmt)?;
    Ok(ToolConfig {
        workspace: file.workspace.map(|workspace| WorkspaceConfig {
            members: workspace.members,
            interface_packages: workspace.interface_packages,
        }),
        lang: file.tool.lang,
        lint_preset: file.lint.preset,
        lint_groups: file.lint.groups,
        lint_severity: file.lint.severity,
        lint_thresholds: file.lint.thresholds,
        lint_options: file.lint.options,
        lint_overrides: file.lint.overrides,
        fmt,
    })
}

fn parse_fmt_config(section: FmtSection) -> Result<FmtConfig, ManifestError> {
    let defaults = FmtConfig::default();
    let max_width = section.max_width.unwrap_or(defaults.max_width);
    if !(40..=200).contains(&max_width) {
        return Err(ManifestError {
            message: format!("fmt.max_width must be between 40 and 200, found {max_width}"),
        });
    }
    let indent = section.indent.unwrap_or(defaults.indent);
    if indent != 2 && indent != 4 {
        return Err(ManifestError {
            message: format!("fmt.indent must be 2 or 4, found {indent}"),
        });
    }
    let end_of_line = match section.end_of_line.as_deref().unwrap_or("lf") {
        "lf" => EndOfLine::Lf,
        "crlf" => EndOfLine::Crlf,
        other => {
            return Err(ManifestError {
                message: format!("fmt.end_of_line must be \"lf\" or \"crlf\", found `{other}`"),
            });
        }
    };
    Ok(FmtConfig {
        max_width,
        indent,
        end_of_line,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_datetimes_are_rejected() {
        let err = parse_manifest(
            r#"
            [plugin]
            name = "datetime"
            version = "0.1.0"
            lang = "1"
            api = "1.0"
            entry = "main.mt"

            [settings]
            when = 1979-05-27T07:32:00Z
            "#,
        )
        .unwrap_err();
        assert!(err.message.contains("TOML datetimes"));
    }

    #[test]
    fn manifest_round_trip() {
        let m = parse_manifest(
            r#"
            [plugin]
            name = "word-count"
            version = "0.2.0"
            lang = "1"
            api = "1.0"
            entry = "main.mt"

            [permissions]
            grant = ["app.editor"]

            [limits]
            fuel = 200_000

            [settings]
            max_words = 5000
            "#,
        )
        .unwrap();
        assert_eq!(m.name, "word-count");
        assert_eq!(m.grants, vec!["app.editor"]);
    }

    #[test]
    fn unknown_key_is_rejected() {
        let err = parse_manifest(
            r#"
            [plugin]
            name = "x"
            version = "1"
            lang = "1"
            api = "1.0"
            entry = "main.mt"
            nope = true
            "#,
        )
        .unwrap_err();
        assert!(err.message.contains("unknown field"));
    }

    #[test]
    fn fmt_defaults_and_values() {
        let config = parse_tool_config("").unwrap();
        assert_eq!(config.fmt, FmtConfig::default());

        let config = parse_tool_config(
            r#"
            [fmt]
            max_width = 80
            indent = 2
            end_of_line = "crlf"
            "#,
        )
        .unwrap();
        assert_eq!(config.fmt.max_width, 80);
        assert_eq!(config.fmt.indent, 2);
        assert_eq!(config.fmt.end_of_line, EndOfLine::Crlf);
    }

    #[test]
    fn format_section_is_a_compatibility_alias() {
        let config = parse_tool_config("[format]\nmax_width = 60\n").unwrap();
        assert_eq!(config.fmt.max_width, 60);
        assert_eq!(config.fmt.indent, 4);
    }

    #[test]
    fn fmt_values_are_validated() {
        for text in [
            "[fmt]\nmax_width = 39\n",
            "[fmt]\nmax_width = 201\n",
            "[fmt]\nindent = 3\n",
            "[fmt]\nend_of_line = \"native\"\n",
        ] {
            assert!(parse_tool_config(text).is_err(), "{text}");
        }
    }

    #[test]
    fn workspace_section_is_parsed() {
        let config = parse_tool_config(
            r#"
            [workspace]
            members = ["plugins/*"]
            interface-packages = ["interfaces/nvlaunch"]

            [lint]
            preset = "strict"
            "#,
        )
        .unwrap();
        let workspace = config.workspace.unwrap();
        assert_eq!(workspace.members, vec!["plugins/*"]);
        assert_eq!(workspace.interface_packages, vec!["interfaces/nvlaunch"]);
        assert_eq!(config.lint_preset.as_deref(), Some("strict"));
    }

    #[test]
    fn workspace_absent_is_distinct_from_empty() {
        assert!(parse_tool_config("").unwrap().workspace.is_none());
        assert!(
            parse_tool_config("[workspace]\n")
                .unwrap()
                .workspace
                .is_some()
        );
    }

    #[test]
    fn unknown_workspace_keys_are_rejected() {
        let err = parse_tool_config("[workspace]\nplugin = [\"x\"]\n").unwrap_err();
        assert!(err.message.contains("unknown field"));
    }

    #[test]
    fn entry_paths_are_validated() {
        for entry in [
            "",
            "/main.mt",
            "\\main.mt",
            "C:/main.mt",
            "a//main.mt",
            "a/./main.mt",
            "a/../main.mt",
            ".hidden/main.mt",
            ".main.mt",
            "dir /main.mt",
            "main.mt ",
            "main.mt.",
            "dir\\..\\main.mt",
        ] {
            assert!(!is_valid_entry_path(entry), "{entry:?}");
            let text = format!(
                "[plugin]\nname = \"x\"\nversion = \"1\"\nlang = \"1\"\napi = \"1.0\"\nentry = {entry:?}\n"
            );
            assert!(parse_manifest(&text).is_err(), "{entry:?}");
        }
        for entry in ["main.mt", "main.v1.mt", "src/main.mt", "dir\\main.mt"] {
            assert!(is_valid_entry_path(entry), "{entry:?}");
        }
    }

    #[test]
    fn interface_manifest_is_parsed() {
        let manifest = parse_interface_manifest(
            r#"
            [interface]
            name = "nvlaunch"
            version = "0.1.0"
            "#,
        )
        .unwrap();
        assert_eq!(manifest.name, "nvlaunch");
        assert_eq!(manifest.version.as_deref(), Some("0.1.0"));
    }

    #[test]
    fn interface_manifest_validates_names() {
        for text in [
            "[interface]\nname = \"NVLaunch\"\n",
            "[interface]\nname = \"open-plugin-folder\"\n",
            "[interface]\n",
            "[interface]\nname = \"nvlaunch\"\nextra = true\n",
        ] {
            assert!(parse_interface_manifest(text).is_err(), "{text}");
        }
        assert!(is_valid_module_segment("std"));
        assert!(is_valid_module_segment("app_2"));
        assert!(!is_valid_module_segment(""));
        assert!(!is_valid_module_segment("2app"));
    }
}
