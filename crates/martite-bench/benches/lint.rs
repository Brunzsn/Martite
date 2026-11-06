use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use martite_base::{LoadLimits, SourceId};
use martite_bench::{fixture, read_fixture};
use martite_config::ToolConfig;
use martite_lint::{LintInputs, SettingsResolver, lint, lint_syntax_only};
use martite_syntax::{ParseOptions, parse_source_with};

fn lint_benches(c: &mut Criterion) {
    let config = ToolConfig {
        lint_preset: Some("recommended".into()),
        ..ToolConfig::default()
    };
    let resolver = SettingsResolver::new(&config, None).expect("lint settings");

    let mut group = c.benchmark_group("lint");
    for (name, fixture_name) in [("clean", "lint/clean.mt"), ("noisy", "lint/noisy.mt")] {
        let (program, sources) = fixture(fixture_name).checked_with_parse(ParseOptions::TOOLING);
        group.bench_function(format!("full_{name}"), |b| {
            b.iter(|| {
                black_box(lint(
                    &program,
                    &sources,
                    &resolver,
                    "",
                    &LintInputs::default(),
                ))
            })
        });

        let path = fixture_name;
        let source = read_fixture(path);
        let settings = resolver.resolve(path);
        let parsed = parse_source_with(&source, LoadLimits::default(), ParseOptions::TOOLING);
        group.bench_function(format!("syntax_only_{name}"), |b| {
            b.iter(|| {
                black_box(lint_syntax_only(
                    SourceId(0),
                    black_box(source.as_str()),
                    &parsed.program,
                    &settings,
                ))
            })
        });
    }
    group.finish();
}

criterion_group!(benches, lint_benches);
criterion_main!(benches);
