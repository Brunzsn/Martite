//! Presets, groups, severities, thresholds, options, and per-path overrides.

use std::collections::{HashMap, HashSet};

use martite_base::{Code, LintGroup};
use martite_config::ToolConfig;

use crate::registry::{self, Level, RULES};

#[derive(Debug, Clone)]
pub struct ConfigError {
    pub message: String,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ConfigError {}

fn error(message: impl Into<String>) -> ConfigError {
    ConfigError {
        message: message.into(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thresholds {
    pub max_fn_lines: i64,
    pub max_params: i64,
    pub max_nesting: i64,
    pub max_branches: i64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            max_fn_lines: 60,
            max_params: 5,
            max_nesting: 4,
            max_branches: 12,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub float_eq_zero_ok: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            float_eq_zero_ok: true,
        }
    }
}

#[derive(Debug, Clone)]
struct Override {
    paths: Vec<String>,
    preset: Option<String>,
    groups: Vec<(LintGroup, Level)>,
    severity: Vec<(Code, Level)>,
    thresholds: ThresholdUpdate,
    options: Option<HashMap<String, toml::Value>>,
    off: Vec<Code>,
}

#[derive(Debug, Clone, Default)]
struct ThresholdUpdate {
    max_fn_lines: Option<i64>,
    max_params: Option<i64>,
    max_nesting: Option<i64>,
    max_branches: Option<i64>,
}

impl ThresholdUpdate {
    fn apply(&self, thresholds: &mut Thresholds) {
        if let Some(value) = self.max_fn_lines {
            thresholds.max_fn_lines = value;
        }
        if let Some(value) = self.max_params {
            thresholds.max_params = value;
        }
        if let Some(value) = self.max_nesting {
            thresholds.max_nesting = value;
        }
        if let Some(value) = self.max_branches {
            thresholds.max_branches = value;
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedSettings {
    levels: HashMap<Code, Level>,
    pub thresholds: Thresholds,
    pub options: Options,
    selected: Option<HashSet<Code>>,
}

impl ResolvedSettings {
    pub fn level(&self, code: Code) -> Level {
        if let Some(selected) = &self.selected {
            if !selected.contains(&code) {
                return Level::Off;
            }
        }
        self.levels.get(&code).copied().unwrap_or(Level::Off)
    }

    pub fn enabled(&self, code: Code) -> bool {
        self.level(code).is_enabled()
    }

    pub fn selected(&self, code: Code) -> bool {
        self.selected
            .as_ref()
            .map(|set| set.contains(&code))
            .unwrap_or(true)
    }
}

#[derive(Debug, Clone)]
pub struct SettingsResolver {
    preset: String,
    groups: Vec<(LintGroup, Level)>,
    severity: Vec<(Code, Level)>,
    thresholds: Thresholds,
    options: Options,
    overrides: Vec<Override>,
    selected: Option<HashSet<Code>>,
}

impl SettingsResolver {
    pub fn new(config: &ToolConfig, selected: Option<HashSet<Code>>) -> Result<Self, ConfigError> {
        let preset = config
            .lint_preset
            .clone()
            .unwrap_or_else(|| "recommended".into());
        validate_preset(&preset)?;

        let mut groups = Vec::new();
        for (name, value) in &config.lint_groups {
            let group = LintGroup::from_str(name)
                .ok_or_else(|| error(format!("unknown lint group `{name}`")))?;
            let level = Level::parse(value).ok_or_else(|| {
                error(format!(
                    "unknown lint severity `{value}` for group `{name}`"
                ))
            })?;
            groups.push((group, level));
        }
        groups.sort_by_key(|(group, _)| *group);

        let mut severity = Vec::new();
        for (name, value) in &config.lint_severity {
            let Some(code) = Code::from_str(name) else {
                return Err(error(format!("unknown lint rule `{name}`")));
            };
            if !code.is_lint() {
                return Err(error(format!("`{name}` is a compile error code")));
            }
            if registry::is_retired(code) {
                continue;
            }
            if !registry::is_known(code) {
                return Err(error(format!("unknown lint rule `{name}`")));
            }
            let level = Level::parse(value)
                .ok_or_else(|| error(format!("unknown lint severity `{value}` for `{name}`")))?;
            severity.push((code, level));
        }
        severity.sort_by_key(|(code, _)| *code);

        let thresholds = thresholds_from(&config.lint_thresholds, Thresholds::default())?;
        let options = options_from(&config.lint_options, Options::default())?;

        let mut overrides = Vec::new();
        for entry in &config.lint_overrides {
            if entry.paths.is_empty() {
                return Err(error(
                    "[[lint.override]] requires at least one `paths` glob",
                ));
            }
            if let Some(preset) = &entry.preset {
                validate_preset(preset)?;
            }
            let mut groups = Vec::new();
            for (name, value) in &entry.groups {
                let group = LintGroup::from_str(name)
                    .ok_or_else(|| error(format!("unknown lint group `{name}`")))?;
                let level = Level::parse(value).ok_or_else(|| {
                    error(format!(
                        "unknown lint severity `{value}` for group `{name}`"
                    ))
                })?;
                groups.push((group, level));
            }
            let mut severity = Vec::new();
            for (name, value) in &entry.severity {
                let Some(code) = Code::from_str(name) else {
                    return Err(error(format!("unknown lint rule `{name}`")));
                };
                if !code.is_lint() {
                    return Err(error(format!("`{name}` is a compile error code")));
                }
                if registry::is_retired(code) {
                    continue;
                }
                if !registry::is_known(code) {
                    return Err(error(format!("unknown lint rule `{name}`")));
                }
                let level = Level::parse(value).ok_or_else(|| {
                    error(format!("unknown lint severity `{value}` for `{name}`"))
                })?;
                severity.push((code, level));
            }
            let mut off = Vec::new();
            for name in &entry.off {
                let Some(code) = Code::from_str(name) else {
                    return Err(error(format!("unknown lint rule `{name}`")));
                };
                if !code.is_lint() {
                    return Err(error(format!("`{name}` is a compile error code")));
                }
                if registry::is_retired(code) {
                    continue;
                }
                if !registry::is_known(code) {
                    return Err(error(format!("unknown lint rule `{name}`")));
                }
                if severity.iter().any(|(id, _)| *id == code) {
                    return Err(error(format!(
                        "`{name}` appears in both `off` and `severity` in one override"
                    )));
                }
                off.push(code);
            }
            let thresholds = threshold_update_from(&entry.thresholds)?;
            let options = if entry.options.is_empty() {
                None
            } else {
                options_from(&entry.options, options)?;
                Some(entry.options.clone())
            };
            overrides.push(Override {
                paths: entry.paths.clone(),
                preset: entry.preset.clone(),
                groups,
                severity,
                thresholds,
                options,
                off,
            });
        }

        Ok(Self {
            preset,
            groups,
            severity,
            thresholds,
            options,
            overrides,
            selected,
        })
    }

    /// Resolve the settings for one file path, relative to the directory
    /// containing `martite.toml` and using `/` separators.
    pub fn resolve(&self, path: &str) -> ResolvedSettings {
        let mut levels = preset_levels(&self.preset);
        apply_groups(&mut levels, &self.groups);
        apply_severity(&mut levels, &self.severity);
        let mut thresholds = self.thresholds;
        let mut options = self.options;

        let path = normalize_path(path);
        for entry in &self.overrides {
            if !entry.paths.iter().any(|glob| glob_match(glob, &path)) {
                continue;
            }
            if let Some(preset) = &entry.preset {
                levels = preset_levels(preset);
                apply_groups(&mut levels, &self.groups);
                apply_severity(&mut levels, &self.severity);
            }
            apply_groups(&mut levels, &entry.groups);
            apply_severity(&mut levels, &entry.severity);
            for code in &entry.off {
                levels.insert(*code, Level::Off);
            }
            entry.thresholds.apply(&mut thresholds);
            if let Some(update) = &entry.options {
                options = options_from(update, options).unwrap_or(options);
            }
        }

        ResolvedSettings {
            levels,
            thresholds,
            options,
            selected: self.selected.clone(),
        }
    }
}

fn validate_preset(preset: &str) -> Result<(), ConfigError> {
    match preset {
        "minimal" | "recommended" | "strict" => Ok(()),
        other => Err(error(format!("unknown lint preset `{other}`"))),
    }
}

fn preset_levels(preset: &str) -> HashMap<Code, Level> {
    let mut levels = HashMap::new();
    for rule in RULES {
        let level = match preset {
            "minimal" => {
                if rule.group == LintGroup::Correct {
                    Level::Warn
                } else {
                    Level::Off
                }
            }
            "strict" => {
                if rule.default == Level::Error {
                    Level::Error
                } else {
                    Level::Warn
                }
            }
            _ => rule.default,
        };
        levels.insert(rule.code, level);
    }
    if preset == "minimal" {
        levels.insert(Code::L010, Level::Error);
        levels.insert(Code::L019, Level::Error);
    }
    levels
}

fn apply_groups(levels: &mut HashMap<Code, Level>, groups: &[(LintGroup, Level)]) {
    for rule in RULES {
        if let Some((_, level)) = groups.iter().find(|(group, _)| *group == rule.group) {
            levels.insert(rule.code, *level);
        }
    }
}

fn apply_severity(levels: &mut HashMap<Code, Level>, severity: &[(Code, Level)]) {
    for (code, level) in severity {
        levels.insert(*code, *level);
    }
}

fn thresholds_from(
    map: &HashMap<String, i64>,
    mut thresholds: Thresholds,
) -> Result<Thresholds, ConfigError> {
    let update = threshold_update_from(map)?;
    update.apply(&mut thresholds);
    Ok(thresholds)
}

fn threshold_update_from(map: &HashMap<String, i64>) -> Result<ThresholdUpdate, ConfigError> {
    let mut update = ThresholdUpdate::default();
    for (name, value) in map {
        match name.as_str() {
            "max_fn_lines" => {
                if *value < 1 {
                    return Err(error("max_fn_lines must be at least 1"));
                }
                update.max_fn_lines = Some(*value);
            }
            "max_params" => {
                if *value < 0 {
                    return Err(error("max_params must be at least 0"));
                }
                update.max_params = Some(*value);
            }
            "max_nesting" => {
                if *value < 0 {
                    return Err(error("max_nesting must be at least 0"));
                }
                update.max_nesting = Some(*value);
            }
            "max_branches" => {
                if *value < 1 {
                    return Err(error("max_branches must be at least 1"));
                }
                update.max_branches = Some(*value);
            }
            other => return Err(error(format!("unknown lint threshold `{other}`"))),
        }
    }
    Ok(update)
}

fn options_from(
    map: &HashMap<String, toml::Value>,
    mut options: Options,
) -> Result<Options, ConfigError> {
    for (name, value) in map {
        match name.as_str() {
            "float_eq_zero_ok" => {
                let Some(value) = value.as_bool() else {
                    return Err(error("float_eq_zero_ok must be a boolean"));
                };
                options.float_eq_zero_ok = value;
            }
            other => return Err(error(format!("unknown lint option `{other}`"))),
        }
    }
    Ok(options)
}

pub fn normalize_path(path: &str) -> String {
    let path = path.replace('\\', "/");
    let mut path = path.as_str();
    while let Some(rest) = path.strip_prefix("./") {
        path = rest;
    }
    path.to_string()
}

/// Match a `martite.toml` path glob: `*` within a segment, `**` across
/// segments, `?` one scalar value. Matching is case-sensitive.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pattern = normalize_path(pattern);
    let path = normalize_path(path);
    let pattern: Vec<&str> = pattern.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    match_segments(&pattern, &path)
}

fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
    let Some((first, rest)) = pattern.split_first() else {
        return path.is_empty();
    };
    if *first == "**" {
        for skip in 0..=path.len() {
            if match_segments(rest, &path[skip..]) {
                return true;
            }
        }
        return false;
    }
    let Some((part, path_rest)) = path.split_first() else {
        return false;
    };
    segment_match(first, part) && match_segments(rest, path_rest)
}

fn segment_match(pattern: &str, value: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let value: Vec<char> = value.chars().collect();
    let mut p = 0usize;
    let mut v = 0usize;
    let mut star: Option<usize> = None;
    let mut star_value = 0usize;
    while v < value.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == value[v]) {
            p += 1;
            v += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            star_value = v;
            p += 1;
        } else if let Some(star_pos) = star {
            p = star_pos + 1;
            star_value += 1;
            v = star_value;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("tests/**", "tests/a/b.mt"));
        assert!(glob_match("tests/*.mt", "tests/a.mt"));
        assert!(!glob_match("tests/*.mt", "tests/a/b.mt"));
        assert!(glob_match("**/*.mt", "a/b.mt"));
        assert!(glob_match("a?c.mt", "abc.mt"));
        assert!(!glob_match("A*.mt", "abc.mt"));
    }

    #[test]
    fn per_path_off_override_from_toml() {
        let config = martite_config::parse_tool_config(
            r#"
            [lint]
            preset = "recommended"

            [lint.groups]
            simplify = "warn"

            [lint.severity]
            L016 = "error"

            [[lint.override]]
            paths = ["main.mt"]
            off = ["L005"]
            "#,
        )
        .unwrap();
        assert_eq!(config.lint_overrides.len(), 1);
        let resolver = SettingsResolver::new(&config, None).unwrap();
        assert_eq!(resolver.resolve("main.mt").level(Code::L005), Level::Off);
        assert_eq!(resolver.resolve("main.mt").level(Code::L016), Level::Error);
    }

    #[test]
    fn presets_and_group_precedence() {
        let mut minimal = ToolConfig::default();
        minimal.lint_preset = Some("minimal".into());
        let resolver = SettingsResolver::new(&minimal, None).unwrap();
        let settings = resolver.resolve("main.mt");
        assert_eq!(settings.level(Code::L003), Level::Warn);
        assert_eq!(settings.level(Code::L001), Level::Off);
        assert_eq!(settings.level(Code::L002), Level::Off);
        assert_eq!(settings.level(Code::L010), Level::Error);

        let mut strict = ToolConfig::default();
        strict.lint_preset = Some("strict".into());
        strict.lint_groups.insert("style".into(), "info".into());
        strict.lint_severity.insert("L016".into(), "error".into());
        let resolver = SettingsResolver::new(&strict, None).unwrap();
        let settings = resolver.resolve("main.mt");
        assert_eq!(settings.level(Code::L016), Level::Error);
        assert_eq!(settings.level(Code::L001), Level::Info);
        assert_eq!(settings.level(Code::L018), Level::Warn);
    }

    #[test]
    fn per_path_off_override() {
        let mut config = ToolConfig::default();
        config.lint_overrides.push(martite_config::LintOverride {
            paths: vec!["main.mt".into()],
            off: vec!["L005".into()],
            ..Default::default()
        });
        let resolver = SettingsResolver::new(&config, None).unwrap();
        assert_eq!(resolver.resolve("main.mt").level(Code::L005), Level::Off);
        assert_eq!(resolver.resolve("other.mt").level(Code::L005), Level::Warn);
    }
}
