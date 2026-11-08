use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use martite_bench::{BENCH_LIMITS, HOST_CALLS_COUNT, fixture, host_calls_plugin};
use martite_runtime::{Runtime, Value};

fn runtime_benches(c: &mut Criterion) {
    let mut group = c.benchmark_group("runtime");
    for name in [
        "arithmetic_loop",
        "fib",
        "function_calls",
        "list_ops",
        "map_ops",
        "string_ops",
        "method_dispatch",
        "pattern_match",
        "closures",
        "captures",
        "mut_params",
        "field_index_ops",
        "option_ops",
        "map_iteration",
    ] {
        let (program, _) = fixture(&format!("runtime/{name}.mt")).checked();
        let mut runtime = Runtime::new(&program, BENCH_LIMITS);
        runtime.run_main().expect("runtime fixture must run");
        group.bench_function(name, |b| b.iter(|| black_box(runtime.run_main())));
    }

    let plugin = fixture("runtime/plugin").load_plugin();
    let mut instance = plugin.instantiate().expect("init");
    let item = Value::str("benchmark");
    group.bench_function("plugin_handler", |b| {
        b.iter(|| black_box(instance.call("saved", vec![item.clone()])))
    });
    group.bench_function("plugin_snapshot", |b| {
        b.iter(|| black_box(instance.snapshot()))
    });

    let snapshot = instance.snapshot();
    let mut restored = plugin.instantiate().expect("init");
    group.bench_function("plugin_restore", |b| {
        b.iter(|| black_box(restored.restore(&snapshot)))
    });

    let host_plugin = host_calls_plugin();
    let mut host_instance = host_plugin.instantiate().expect("init");
    group.bench_function("host_calls", |b| {
        b.iter(|| {
            black_box(host_instance.call("run", vec![Value::Int(HOST_CALLS_COUNT), Value::Int(7)]))
        })
    });
    group.finish();
}

criterion_group!(benches, runtime_benches);
criterion_main!(benches);
