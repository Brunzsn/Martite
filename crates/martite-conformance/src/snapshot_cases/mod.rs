//! Shared runtime cases used by the snapshot oracle and the test suite.

use crate::snapshot::{HostSet, PluginScenario, SnapshotCase};
use crate::{Case, ProbeCaps};

pub mod bench;
pub mod closures;
pub mod e2e;
pub mod host;
pub mod limits;
pub mod patterns;
pub mod plugin_errors;
pub mod plugins;
pub mod traps;

pub fn all_cases() -> Vec<SnapshotCase> {
    let mut cases = Vec::new();
    cases.extend(bench::cases());
    cases.extend(e2e::cases());
    cases.extend(limits::cases());
    cases.extend(traps::cases());
    cases.extend(closures::cases());
    cases.extend(patterns::cases());
    cases.extend(plugins::cases());
    cases.extend(host::cases());
    cases.extend(plugin_errors::cases());
    cases.sort_by(|left, right| left.id().cmp(right.id()));
    cases
}

pub(crate) fn default_caps() -> ProbeCaps {
    ProbeCaps::default()
}

pub(crate) fn bench_caps() -> ProbeCaps {
    ProbeCaps {
        fuel: 100_000_000,
        memory: 64 << 20,
        depth: 200,
    }
}

pub(crate) fn main_case(
    id: impl Into<String>,
    source: impl Into<String>,
    hosts: Option<HostSet>,
    caps: ProbeCaps,
) -> SnapshotCase {
    SnapshotCase::Main {
        id: id.into(),
        case: Case::single(source),
        hosts,
        caps,
    }
}

pub(crate) fn main_case_files(
    id: impl Into<String>,
    files: Vec<(String, String)>,
    entry: &str,
    hosts: Option<HostSet>,
    caps: ProbeCaps,
) -> SnapshotCase {
    SnapshotCase::Main {
        id: id.into(),
        case: Case {
            files,
            entry: entry.into(),
            interfaces: Vec::new(),
            grants: Vec::new(),
            limits: martite_base::LoadLimits::default(),
            edition: "1".into(),
        },
        hosts,
        caps,
    }
}

pub(crate) fn plugin_case(
    id: impl Into<String>,
    manifest: impl Into<String>,
    files: Vec<(String, String)>,
    hosts: Option<HostSet>,
    steps: Vec<crate::snapshot::PluginStep>,
    caps: ProbeCaps,
) -> SnapshotCase {
    let id = id.into();
    SnapshotCase::Plugin {
        scenario: PluginScenario {
            id: id.clone(),
            manifest: manifest.into(),
            files,
            hosts,
            steps,
        },
        id,
        caps,
    }
}
