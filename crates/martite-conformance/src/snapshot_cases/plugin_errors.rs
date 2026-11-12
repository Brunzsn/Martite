use crate::snapshot::SnapshotCase;

/// `plugin_errors` exercises load-time diagnostics (E501-E509). Those programs
/// do not produce a checked program, so there is no runtime behavior to
/// snapshot. The executable plugin cases live in `snapshot_cases::plugins`.
pub fn cases() -> Vec<SnapshotCase> {
    Vec::new()
}
