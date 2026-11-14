//! Optimizer-layer benchmarks.
//!
//! This target deliberately avoids parsing, checking, loading and CLI work. It
//! times lowering from the checked AST into IR, the optimizer pass, and
//! execution of the lowered versus optimized program.
//!
//! The `runtime_lowered`/`runtime_optimized` pair is the optimizer delta. The
//! separate Optimizer Benchmarks Comparison workflow reports both stages
//! against a baseline revision.

use std::hint::black_box;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use martite_bench::{BENCH_LIMITS, fixture};
use martite_driver::{LowerOptions, lower_with};
use martite_opt::{OptimizeOptions, optimize};
use martite_runtime::IrRuntime;

fn optimizer_benches(c: &mut Criterion) {
    let (fold_program, fold_sources) = fixture("optimize/constant_fold.mt").checked();
    let unoptimized = lower_with(&fold_program, &fold_sources, LowerOptions { verify: false })
        .program
        .expect("optimizer fixture lowers");
    let mut optimized = unoptimized.clone();
    optimize(&mut optimized, OptimizeOptions::default()).expect("optimizer fixture optimizes");

    let corpus: Vec<_> = [
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
    ]
    .into_iter()
    .map(|name| fixture(&format!("runtime/{name}.mt")).checked())
    .collect();
    let lowered_corpus: Vec<_> = corpus
        .iter()
        .map(|(program, sources)| {
            lower_with(program, sources, LowerOptions { verify: false })
                .program
                .expect("runtime fixture lowers")
        })
        .collect();

    let mut group = c.benchmark_group("optimizer");
    group.bench_function("lower_const_fold", |b| {
        b.iter(|| {
            black_box(
                lower_with(&fold_program, &fold_sources, LowerOptions { verify: false })
                    .program
                    .expect("optimizer fixture lowers"),
            )
        })
    });
    group.bench_function("lower_corpus", |b| {
        b.iter(|| {
            let lowered: Vec<_> = corpus
                .iter()
                .map(|(program, sources)| {
                    lower_with(program, sources, LowerOptions { verify: false })
                        .program
                        .expect("runtime fixture lowers")
                })
                .collect();
            black_box(lowered)
        })
    });
    group.bench_function("pass_const_fold", |b| {
        b.iter_batched(
            || unoptimized.clone(),
            |mut ir| {
                optimize(&mut ir, OptimizeOptions::default()).expect("optimize");
                black_box(ir)
            },
            BatchSize::SmallInput,
        )
    });
    group.bench_function("pass_corpus", |b| {
        b.iter_batched(
            || lowered_corpus.clone(),
            |mut programs| {
                for ir in &mut programs {
                    optimize(ir, OptimizeOptions::default()).expect("optimize");
                }
                black_box(programs)
            },
            BatchSize::SmallInput,
        )
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
