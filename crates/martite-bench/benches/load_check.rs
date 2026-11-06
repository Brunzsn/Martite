use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use martite_bench::fixture;
use martite_driver::load;

fn load_check(c: &mut Criterion) {
    let mut group = c.benchmark_group("load");
    for (name, fixture_name) in [
        ("single", "load/single"),
        ("multi_module", "load/multi"),
        ("plugin_interface", "load/plugin"),
    ] {
        let loaded = fixture(fixture_name);
        let provider = loaded.provider();
        let options = loaded.load_options();
        assert!(
            !load(&provider, &options).has_errors(),
            "load fixture `{fixture_name}` must be clean"
        );
        group.bench_function(name, |b| {
            b.iter(|| black_box(load(black_box(&provider), black_box(&options))))
        });
    }
    group.finish();
}

criterion_group!(benches, load_check);
criterion_main!(benches);
