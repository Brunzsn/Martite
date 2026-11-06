use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use martite_base::LoadLimits;
use martite_bench::read_fixture;
use martite_syntax::lex;

fn lexer(c: &mut Criterion) {
    let limits = LoadLimits::default();
    let mut group = c.benchmark_group("lexer");
    for (name, fixture) in [
        ("small", "syntax/small.mt"),
        ("medium", "syntax/medium.mt"),
        ("large", "syntax/large.mt"),
        ("strings_interpolation", "syntax/strings.mt"),
        ("comments", "syntax/comments.mt"),
    ] {
        let source = read_fixture(fixture);
        group.bench_function(name, |b| {
            b.iter(|| black_box(lex(black_box(source.as_str()), limits)))
        });
    }
    group.finish();
}

criterion_group!(benches, lexer);
criterion_main!(benches);
