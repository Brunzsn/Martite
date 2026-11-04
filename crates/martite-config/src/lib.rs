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
    if entry.is_empty()
        || entry.starts_with('/')
        || entry.starts_with('\\')
        || entry
            .split(['/', '\\'])
            .any(|part| part.is_empty() || part == ".." || part == ".")
        || entry.contains(':')
    {
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
    pub tool: ToolSection,
    #[serde(default)]
    pub lint: LintSection,
    #[serde(default)]
    pub format: FormatSection,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ToolSection {
    pub lang: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct LintSection {
    #[serde(default)]
    pub severity: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub threshold: std::collections::HashMap<String, i64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct FormatSection {
    pub max_width: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct ToolConfig {
    pub lang: Option<String>,
    pub lint_severity: std::collections::HashMap<String, String>,
    pub lint_threshold: std::collections::HashMap<String, i64>,
    pub max_width: Option<usize>,
}

pub fn parse_tool_config(text: &str) -> Result<ToolConfig, ManifestError> {
    let file: ToolFile = toml::from_str(text).map_err(|e| ManifestError {
        message: format!("malformed martite.toml: {e}"),
    })?;
    Ok(ToolConfig {
        lang: file.tool.lang,
        lint_severity: file.lint.severity,
        lint_threshold: file.lint.threshold,
        max_width: file.format.max_width,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
