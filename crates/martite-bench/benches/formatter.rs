use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use martite_bench::{formatter_corpus_root, read_fixture, walk_files};
use martite_config::FmtConfig;
use martite_fmt::{format_interface, format_source};

fn formatter(c: &mut Criterion) {
    let config = FmtConfig::default();
    let mut group = c.benchmark_group("fmt");

    for (name, fixture, interface) in [
        ("medium", "syntax/medium.mt", false),
        ("large", "syntax/large.mt", false),
        ("interface", "syntax/interface.mti", true),
    ] {
        let source = read_fixture(fixture);
        group.bench_function(name, |b| {
            b.iter(|| {
                let output = if interface {
                    format_interface(black_box(source.as_str()), &config)
                } else {
                    format_source(black_box(source.as_str()), &config)
                };
                black_box(output.text)
            })
        });
    }

    let mut corpus = Vec::new();
    for path in walk_files(&formatter_corpus_root()) {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if !name.ends_with(".before.mt") && !name.ends_with(".before.mti") {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("read formatter fixture");
        corpus.push((name.ends_with(".mti"), source));
    }
    group.bench_function("corpus", |b| {
        b.iter(|| {
            let mut total = 0;
            for (interface, source) in &corpus {
                let output = if *interface {
                    format_interface(black_box(source.as_str()), &config)
                } else {
                    format_source(black_box(source.as_str()), &config)
                };
                total += output.text.map(|text| text.len()).unwrap_or(0);
            }
            black_box(total)
        })
    });

    let already_formatted = read_fixture("syntax/medium.mt");
    let formatted = format_source(&already_formatted, &config)
        .text
        .expect("medium fixture formats");
    group.bench_function("idempotent", |b| {
        b.iter(|| black_box(format_source(black_box(formatted.as_str()), &config)))
    });
    group.bench_function("check", |b| {
        b.iter(|| {
            let output = format_source(black_box(already_formatted.as_str()), &config);
            black_box(output.text.as_deref() == Some(already_formatted.as_str()))
        })
    });
    group.finish();
}

criterion_group!(benches, formatter);
criterion_main!(benches);
