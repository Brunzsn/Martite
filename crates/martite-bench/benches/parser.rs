use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use martite_base::LoadLimits;
use martite_bench::read_fixture;
use martite_syntax::{parse_interface, parse_source};

fn parser(c: &mut Criterion) {
    let limits = LoadLimits::default();
    let mut group = c.benchmark_group("parser");
    for (name, fixture) in [
        ("small", "syntax/small.mt"),
        ("medium", "syntax/medium.mt"),
        ("large", "syntax/large.mt"),
    ] {
        let source = read_fixture(fixture);
        group.bench_function(name, |b| {
            b.iter(|| black_box(parse_source(black_box(source.as_str()), limits)))
        });
    }

    let interface = read_fixture("syntax/interface.mti");
    group.bench_function("interface", |b| {
        b.iter(|| black_box(parse_interface(black_box(interface.as_str()), limits)))
    });

    let deep = read_fixture("parser/deep_expr.mt");
    group.bench_function("deep_expression", |b| {
        b.iter(|| black_box(parse_source(black_box(deep.as_str()), limits)))
    });

    let malformed = read_fixture("parser/malformed.mt");
    group.bench_function("error_recovery", |b| {
        b.iter(|| black_box(parse_source(black_box(malformed.as_str()), limits)))
    });
    group.finish();
}

criterion_group!(benches, parser);
criterion_main!(benches);
