use std::ffi::OsStr;
use std::hint::black_box;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use criterion::{Criterion, criterion_group, criterion_main};
use martite_bench::{cli_binary, corpus_root, formatter_corpus_root};

fn run(bin: &PathBuf, cwd: &PathBuf, args: &[&OsStr]) {
    let status = Command::new(bin)
        .current_dir(cwd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run martite");
    black_box(status);
}

fn cli(c: &mut Criterion) {
    let bin = cli_binary();
    let corpus = corpus_root();
    let workspace = martite_bench::workspace_root();
    let fmt_check = formatter_corpus_root().join("worked/after.mt");
    let fmt_check = fmt_check.to_string_lossy().to_string();

    let mut group = c.benchmark_group("cli");
    group.sample_size(20);
    group.bench_function("check_small", |b| {
        b.iter(|| {
            run(
                &bin,
                &corpus,
                &[OsStr::new("check"), OsStr::new("syntax/small.mt")],
            )
        })
    });
    group.bench_function("lint_small", |b| {
        b.iter(|| {
            run(
                &bin,
                &corpus,
                &[OsStr::new("lint"), OsStr::new("lint/clean.mt")],
            )
        })
    });
    group.bench_function("run_small", |b| {
        b.iter(|| {
            run(
                &bin,
                &corpus,
                &[OsStr::new("run"), OsStr::new("syntax/small.mt")],
            )
        })
    });
    group.bench_function("fmt_check", |b| {
        b.iter(|| {
            run(
                &bin,
                &workspace,
                &[
                    OsStr::new("fmt"),
                    OsStr::new("--check"),
                    OsStr::new(&fmt_check),
                ],
            )
        })
    });
    group.finish();
}

criterion_group!(benches, cli);
criterion_main!(benches);
