//! Optimizer-layer benchmarks.
//!
//! This target deliberately avoids parsing, checking, loading and CLI work. The
//! fixture is lowered once in setup, then the benchmark times:
//!
//! - the `const-fold` pass (including a clone of the lowered program),
//! - execution of the lowered program,
//! - execution of the optimized program.
//!
//! The `runtime_lowered`/`runtime_optimized` pair is the optimizer delta. The
//! separate Optimizer Benchmarks Comparison workflow reports both stages
//! against a baseline revision.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use martite_bench::{BENCH_LIMITS, fixture};
use martite_opt::{OptimizeOptions, optimize};
use martite_runtime::IrRuntime;

fn optimizer_benches(c: &mut Criterion) {
    let lowered = fixture("optimize/constant_fold.mt").lowered();
    let unoptimized = lowered.program.expect("optimizer fixture lowers");
    let mut optimized = unoptimized.clone();
    optimize(&mut optimized, OptimizeOptions::default()).expect("optimizer fixture optimizes");

    let mut group = c.benchmark_group("optimizer");
    group.bench_function("pass_const_fold", |b| {
        b.iter(|| {
            let mut ir = unoptimized.clone();
            optimize(&mut ir, OptimizeOptions::default()).expect("optimize");
            black_box(ir)
        })
    });

    let mut lowered_runtime = IrRuntime::new(&unoptimized, BENCH_LIMITS);
    lowered_runtime.run_main().expect("lowered fixture runs");
    group.bench_function("runtime_lowered", |b| {
        b.iter(|| black_box(lowered_runtime.run_main()))
    });

    let mut optimized_runtime = IrRuntime::new(&optimized, BENCH_LIMITS);
    optimized_runtime
        .run_main()
        .expect("optimized fixture runs");
    group.bench_function("runtime_optimized", |b| {
        b.iter(|| black_box(optimized_runtime.run_main()))
    });
    group.finish();
}

criterion_group!(benches, optimizer_benches);
criterion_main!(benches);
