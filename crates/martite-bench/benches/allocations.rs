use std::alloc::System;
use std::fmt::Write as _;
use std::fs;
use std::hint::black_box;

use martite_bench::{BENCH_LIMITS, HOST_CALLS_COUNT, criterion_dir, fixture, host_calls_plugin};
use martite_runtime::{IrRuntime, Value};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, Stats, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

fn report(table: &mut String, name: &str, iterations: u64, stats: Stats) {
    let per = |value: usize| value as u64 / iterations.max(1);
    writeln!(
        table,
        "| {name} | {} | {} | {} | {} |",
        per(stats.allocations),
        per(stats.bytes_allocated),
        per(stats.reallocations),
        per(stats.deallocations),
    )
    .expect("format allocation row");
}

fn measure<F>(table: &mut String, name: &str, iterations: u64, mut op: F)
where
    F: FnMut(),
{
    for _ in 0..3 {
        op();
    }
    let region = Region::new(GLOBAL);
    for _ in 0..iterations {
        op();
    }
    report(table, name, iterations, region.change());
}

fn runtime_alloc(table: &mut String, name: &str, fixture_name: &str, iterations: u64) {
    let lowered = fixture(fixture_name).lowered();
    let ir = lowered.program.expect("runtime fixture lowers");
    let mut runtime = IrRuntime::new(&ir, BENCH_LIMITS);
    runtime.run_main().expect("runtime fixture must run");
    measure(table, name, iterations, || {
        let _ = black_box(runtime.run_main());
    });
}

fn host_call_allocs(table: &mut String) {
    let plugin = host_calls_plugin();
    let mut instance = plugin.instantiate().expect("init");
    measure(table, "runtime/host_calls", 20, || {
        let _ = black_box(instance.call("run", vec![Value::Int(HOST_CALLS_COUNT), Value::Int(7)]));
    });
}

fn plugin_allocs(table: &mut String) {
    let plugin = fixture("runtime/plugin").load_plugin();
    let mut instance = plugin.instantiate().expect("init");
    let item = Value::str("benchmark");
    measure(table, "runtime/plugin_handler", 20, || {
        let _ = black_box(instance.call("saved", vec![item.clone()]));
    });
    measure(table, "runtime/plugin_snapshot", 20, || {
        let _ = black_box(instance.snapshot());
    });

    let snapshot = instance.snapshot();
    let mut restored = plugin.instantiate().expect("init");
    measure(table, "runtime/plugin_restore", 20, || {
        let _ = black_box(restored.restore(&snapshot));
    });
}

fn main() {
    let mut table = String::new();
    table.push_str("| benchmark | allocations | bytes | reallocations | deallocations |\n");
    table.push_str("| --- | ---: | ---: | ---: | ---: |\n");
    for (name, fixture_name, iterations) in [
        ("runtime/arithmetic_loop", "runtime/arithmetic_loop.mt", 20),
        ("runtime/fib", "runtime/fib.mt", 10),
        ("runtime/function_calls", "runtime/function_calls.mt", 20),
        ("runtime/list_ops", "runtime/list_ops.mt", 20),
        ("runtime/map_ops", "runtime/map_ops.mt", 20),
        ("runtime/string_ops", "runtime/string_ops.mt", 20),
        ("runtime/method_dispatch", "runtime/method_dispatch.mt", 20),
        ("runtime/pattern_match", "runtime/pattern_match.mt", 20),
        ("runtime/closures", "runtime/closures.mt", 20),
        ("runtime/captures", "runtime/captures.mt", 20),
        ("runtime/mut_params", "runtime/mut_params.mt", 20),
        ("runtime/field_index_ops", "runtime/field_index_ops.mt", 20),
        ("runtime/option_ops", "runtime/option_ops.mt", 20),
        ("runtime/map_iteration", "runtime/map_iteration.mt", 20),
    ] {
        runtime_alloc(&mut table, name, fixture_name, iterations);
    }
    host_call_allocs(&mut table);
    plugin_allocs(&mut table);

    println!();
    println!("Allocation report (per iteration, counting allocator)");
    println!();
    print!("{table}");

    let directory = criterion_dir();
    fs::create_dir_all(&directory).expect("create criterion directory");
    fs::write(directory.join("allocations.md"), &table).expect("write allocation report");
}
