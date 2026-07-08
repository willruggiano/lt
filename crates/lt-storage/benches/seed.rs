//! Throughput of [`lt_storage::fake::seed`] against a fresh, file-backed
//! SQLite database.
//!
//! Criterion's `Bencher` closures cannot propagate a `Result` (there is no
//! fallible `iter`/`iter_batched` variant), so setup/teardown failures here
//! are reported the same way `build.rs` reports them: by panicking. See
//! `crates/lt-storage/build.rs`'s identical exemption.
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use anyhow::Result;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use lt_storage::db::{Connection, Storage, open_db};
use lt_storage::fake::{self, Dataset};

/// A file-backed database rooted in a fresh temporary directory per
/// `iter_batched` sample, so each sample seeds an empty schema rather than
/// accumulating rows across samples.
struct TempSqlite {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl TempSqlite {
    fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("bench.db");
        Ok(Self { _dir: dir, path })
    }
}

impl Storage for TempSqlite {
    fn connect(&self) -> Result<Connection> {
        open_db(&self.path)
    }
}

fn bench_seed(c: &mut Criterion) {
    let dataset = Dataset {
        teams: 5,
        users: 50,
        issues: 2_000,
        comments: 20_000,
    };

    let mut group = c.benchmark_group("fake_seed");
    group.sample_size(10);
    group.bench_function("sqlite", |b| {
        b.iter_batched(
            || TempSqlite::new().unwrap(),
            |mut storage| fake::seed(&mut storage, dataset).unwrap(),
            BatchSize::LargeInput,
        );
    });
    group.finish();
}

criterion_group!(benches, bench_seed);
criterion_main!(benches);
