// SPDX-License-Identifier: MPL-2.0
use std::collections::BTreeMap;
use std::hint::black_box;
use std::path::PathBuf;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use tempfile::TempDir;
use transist::{TransistContext, TransistInspectionContext};
use transist_core::ports::RunStorePort;
use transist_core::{KernelRunSnapshot, PersistedRunRecord, RunLineage, RunLineageMode};
use transist_storage_local::{LocalRunStore, LocalRunSummaryStore, LocalStorageConfig};

const ROOT_RUN_ID: &str = "tui-fixture-root";
const CHILD_COUNT: usize = 32;
const BENCHMARK_CACHE_CAPACITY: usize = 0;

struct TuiFixture {
    _home: TempDir,
    repo_root: PathBuf,
    child_run_ids: Vec<String>,
}

impl TuiFixture {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("create benchmark home");
        let store = LocalRunStore::new(LocalStorageConfig::new(home.path()));
        let root_lineage = RunLineage {
            mode: RunLineageMode::Fresh,
            source_run_id: None,
            root_run_id: ROOT_RUN_ID.to_string(),
            parent_run_id: None,
            parent_step_id: None,
        };
        store
            .create_run(&record(ROOT_RUN_ID, root_lineage))
            .expect("create root fixture run");

        let mut child_run_ids = Vec::with_capacity(CHILD_COUNT);
        for index in 0..CHILD_COUNT {
            let run_id = format!("{ROOT_RUN_ID}__child_{index:03}");
            let lineage = RunLineage {
                mode: RunLineageMode::Fresh,
                source_run_id: None,
                root_run_id: ROOT_RUN_ID.to_string(),
                parent_run_id: Some(ROOT_RUN_ID.to_string()),
                parent_step_id: Some("inspect_children".to_string()),
            };
            store
                .create_run(&record(&run_id, lineage))
                .expect("create child fixture run");
            child_run_ids.push(run_id);
        }

        let summaries = LocalRunSummaryStore::new(home.path());
        summaries
            .refresh_run(ROOT_RUN_ID)
            .expect("refresh root fixture summary");
        for run_id in &child_run_ids {
            summaries
                .refresh_run(run_id)
                .expect("refresh child fixture summary");
        }

        Self {
            _home: home,
            repo_root: PathBuf::from("benchmark-repository"),
            child_run_ids,
        }
    }

    fn inspection(&self) -> TransistInspectionContext {
        TransistContext::with_home_and_inspection_cache_capacity(
            self.repo_root.clone(),
            self._home.path().to_path_buf(),
            BENCHMARK_CACHE_CAPACITY,
        )
        .inspection()
    }
}

fn record(run_id: &str, lineage: RunLineage) -> PersistedRunRecord {
    let snapshot = KernelRunSnapshot::new(
        run_id,
        "tui_load_fixture",
        "v1",
        &["inspect_children".to_string()],
    );
    PersistedRunRecord::from_snapshot(snapshot, None, Some(lineage), BTreeMap::new(), 0, 1, 2)
}

fn benchmark_tui_run_detail_loading(c: &mut Criterion) {
    let fixture = TuiFixture::new();
    let mut group = c.benchmark_group("tui_run_detail_loading");

    group.bench_function("sequential_child_loads", |b| {
        b.iter_batched(
            || fixture.inspection(),
            |inspection| {
                let loaded_ids = fixture
                    .child_run_ids
                    .iter()
                    .filter_map(|run_id| {
                        inspection
                            .get_run(black_box(run_id))
                            .expect("load child run")
                            .map(|detail| detail.summary.run_id)
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    loaded_ids, fixture.child_run_ids,
                    "sequential results must match requested child IDs"
                );
                black_box(loaded_ids);
            },
            BatchSize::PerIteration,
        );
    });

    group.bench_function("batched_child_loads", |b| {
        b.iter_batched(
            || fixture.inspection(),
            |inspection| {
                let loaded = inspection
                    .get_runs(black_box(&fixture.child_run_ids))
                    .expect("batch load child runs");
                let loaded_ids = loaded.keys().cloned().collect::<Vec<_>>();
                assert_eq!(
                    loaded_ids, fixture.child_run_ids,
                    "batched results must match requested child IDs"
                );
                black_box(loaded);
            },
            BatchSize::PerIteration,
        );
    });

    group.finish();
}

criterion_group!(benches, benchmark_tui_run_detail_loading);
criterion_main!(benches);
