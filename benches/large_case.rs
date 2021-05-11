// SPDX-License-Identifier: MPL-2.0
use std::time::Duration;

extern crate criterion;
use self::criterion::*;

use pubgrub::package::Package;
use pubgrub::range::Range;
use pubgrub::range::RangeSet;
use pubgrub::solver::{resolve, OfflineDependencyProvider};
use pubgrub::version::{NumberVersion, SemanticVersion};
use serde::de::Deserialize;

fn bench<'a, P, R>(b: &mut Bencher, case: &'a str)
where
    P: Package + Deserialize<'a>,
    R: RangeSet + Deserialize<'a>,
    R::VERSION: Deserialize<'a>,
{
    let dependency_provider: OfflineDependencyProvider<P, R> = ron::de::from_str(&case).unwrap();

    b.iter(|| {
        for p in dependency_provider.packages() {
            for n in dependency_provider.versions(p).unwrap() {
                let _ = resolve(&dependency_provider, p.clone(), n.clone());
            }
        }
    });
}

fn bench_nested(c: &mut Criterion) {
    let mut group = c.benchmark_group("large_cases");
    group.measurement_time(Duration::from_secs(20));

    for case in std::fs::read_dir("test-examples").unwrap() {
        let case = case.unwrap().path();
        let name = case.file_name().unwrap().to_string_lossy();
        let data = std::fs::read_to_string(&case).unwrap();
        if name.ends_with("u16_NumberVersion.ron") {
            group.bench_function(name, |b| {
                bench::<u16, Range<NumberVersion>>(b, &data);
            });
        } else if name.ends_with("str_SemanticVersion.ron") {
            group.bench_function(name, |b| {
                bench::<&str, Range<SemanticVersion>>(b, &data);
            });
        }
    }

    group.finish();
}

criterion_group!(benches, bench_nested);
criterion_main!(benches);
