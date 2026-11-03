// SPDX-License-Identifier: MPL-2.0
use super::{IndexedBlob, IndexedRuntimeEvent};
use crate::run_summary::PersistedRunSummaryRecord;
use anyhow::{Result, anyhow};
use lru::LruCache;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use transist_core::PersistedRunRecord;

/// Environment variable that disables inspection caching when set to `off`.
pub const INSPECTION_CACHE_DISABLE_ENV: &str = "TRANSIST_INSPECTION_CACHE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Lightweight fingerprint used to detect file changes cheaply.
pub struct FileFingerprint {
    /// File modification time represented as nanoseconds since Unix epoch.
    pub mtime_nanos: i128,
    /// File length in bytes.
    pub len: u64,
}

/// Content identity for the small summary projection, independent of filesystem
/// timestamp granularity, atomic replacement, and inode reuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SummaryFingerprint([u8; 32]);

impl SummaryFingerprint {
    /// Hashes captured summary bytes; the cache loader must parse these same
    /// bytes. Performs no file I/O and never hashes artifact/evidence payloads.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }
}

/// Identifies one cached run without allowing different Transist homes to collide.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InspectionCacheKey {
    /// Canonical Transist home directory containing the run.
    pub home_dir: PathBuf,
    /// Persisted run identifier within the home directory.
    pub run_id: String,
}

impl InspectionCacheKey {
    /// Builds a key from a home directory and run identifier.
    pub fn new(home_dir: &Path, run_id: impl Into<String>) -> Result<Self> {
        let home_dir = fs::canonicalize(home_dir)
            .map_err(|error| anyhow!("failed canonicalizing inspection home: {}", error))?;
        Ok(Self {
            home_dir,
            run_id: run_id.into(),
        })
    }
}

impl FileFingerprint {
    /// Builds a fingerprint from file metadata, or `None` if the path does not exist.
    pub fn from_path(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let metadata = fs::metadata(path)?;
        let mtime_nanos = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos() as i128)
            .unwrap_or(0);
        Ok(Some(Self {
            mtime_nanos,
            len: metadata.len(),
        }))
    }
}

#[derive(Debug, Clone)]
/// Cached parsed runtime events for one run.
pub struct EventsCacheEntry {
    /// Fingerprint of the backing `events.jsonl` file when this value was parsed.
    pub fingerprint: FileFingerprint,
    /// Parsed runtime events memoized for the fingerprint.
    pub parsed: Arc<Vec<IndexedRuntimeEvent>>,
}

#[derive(Debug, Clone)]
pub struct BlobListingCacheEntry {
    /// Fingerprint of the artifacts root when this listing was built.
    pub fingerprint: FileFingerprint,
    /// Parsed artifact/evidence listing memoized for the fingerprint.
    pub blobs: Arc<Vec<IndexedBlob>>,
}

#[derive(Debug, Clone, Default)]
/// Per-run cache slot containing memoized inspection payloads.
pub struct RunCacheSlot {
    /// Cached `state.json` record.
    pub state: Option<(FileFingerprint, Arc<PersistedRunRecord>)>,
    /// Cached `events.jsonl` stream.
    pub events: Option<EventsCacheEntry>,
    /// Cached `summary.json` record.
    pub summary: Option<(SummaryFingerprint, Arc<PersistedRunSummaryRecord>)>,
    /// Cached artifacts listing.
    pub artifact_listing: Option<BlobListingCacheEntry>,
    /// Cached evidence listing.
    pub evidence_listing: Option<BlobListingCacheEntry>,
}

#[derive(Debug, Clone, Copy, Default)]
/// Aggregate cache counters for diagnostics.
pub struct InspectionCacheStats {
    /// Number of cache hits.
    pub hits: u64,
    /// Number of cache misses.
    pub misses: u64,
    /// Number of LRU evictions.
    pub evictions: u64,
}

/// In-memory run-scoped inspection cache with LRU eviction.
#[derive(Debug)]
pub struct InspectionCache {
    capacity: usize,
    slots: Mutex<Option<LruCache<InspectionCacheKey, RunCacheSlot>>>,
    in_flight: (Mutex<HashSet<InspectionCacheKey>>, Condvar),
    mutations: Mutex<CacheMutations>,
    hits: AtomicU64,
    misses: AtomicU64,
    evictions: AtomicU64,
}

impl InspectionCache {
    /// Resolves requested capacity after applying environment overrides.
    pub fn resolve_capacity(requested_capacity: usize) -> usize {
        let disabled = std::env::var(INSPECTION_CACHE_DISABLE_ENV)
            .ok()
            .map(|value| value.trim().eq_ignore_ascii_case("off"))
            .unwrap_or(false);
        if disabled { 0 } else { requested_capacity }
    }

    /// Builds a cache using `requested_capacity` and env-based overrides.
    pub fn with_default_capacity(requested_capacity: usize) -> Self {
        Self::new(Self::resolve_capacity(requested_capacity))
    }

    /// Creates a new cache with a run-slot capacity.
    pub fn new(capacity: usize) -> Self {
        let slots = NonZeroUsize::new(capacity).map(LruCache::new);
        Self {
            capacity,
            slots: Mutex::new(slots),
            in_flight: (Mutex::new(HashSet::new()), Condvar::new()),
            mutations: Mutex::new(CacheMutations::default()),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
        }
    }

    /// Returns configured run-slot capacity.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns point-in-time hit/miss/eviction counters.
    pub fn stats(&self) -> InspectionCacheStats {
        InspectionCacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
        }
    }

    /// Returns the cached `state.json` fingerprint for a run key, if present.
    pub fn state_fingerprint(&self, key: &InspectionCacheKey) -> Option<FileFingerprint> {
        let mut guard = self.lock_slots();
        let lru = guard.as_mut()?;
        lru.get(key)
            .and_then(|slot| slot.state.as_ref().map(|(fp, _)| *fp))
    }

    /// Clears all cached run slots.
    pub fn clear_all(&self) {
        let mut mutations = self.lock_mutations();
        mutations.clear_epoch = mutations.clear_epoch.wrapping_add(1);
        let mut guard = self.lock_slots();
        if let Some(lru) = guard.as_mut() {
            lru.clear();
        }
    }

    /// Removes one run slot from cache.
    pub fn invalidate(&self, key: &InspectionCacheKey) {
        let mut mutations = self.lock_mutations();
        let generation = mutations.key_generations.entry(key.clone()).or_default();
        *generation = generation.wrapping_add(1);
        let mut guard = self.lock_slots();
        if let Some(lru) = guard.as_mut() {
            lru.pop(key);
        }
    }

    /// Generic read-through helper for a run cache slot.
    pub fn with_run_slot<T>(
        &self,
        key: &InspectionCacheKey,
        mut read: impl FnMut(&RunCacheSlot) -> Option<T>,
        mut fill: impl FnMut(&mut RunCacheSlot) -> Result<T>,
    ) -> Result<T> {
        if self.capacity == 0 {
            self.misses.fetch_add(1, Ordering::Relaxed);
            let mut tmp = RunCacheSlot::default();
            return fill(&mut tmp);
        }

        loop {
            {
                let mut guard = self.lock_slots();
                let lru = guard
                    .as_mut()
                    .expect("inspection cache capacity invariant violated");
                if let Some(slot) = lru.get_mut(key)
                    && let Some(value) = read(slot)
                {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    return Ok(value);
                }
            }

            let mut in_flight = self.lock_in_flight()?;
            if !in_flight.insert(key.clone()) {
                while in_flight.contains(key) {
                    in_flight = match self.in_flight.1.wait(in_flight) {
                        Ok(guard) => guard,
                        Err(_) => {
                            return Err(anyhow!("inspection cache in-flight state is poisoned"));
                        }
                    };
                }
                continue;
            }
            drop(in_flight);

            let _flight = InFlightGuard::new(self, key.clone());
            self.misses.fetch_add(1, Ordering::Relaxed);
            let generation = self.generation_for(key);
            let mut slot = self
                .lock_slots()
                .as_ref()
                .and_then(|lru| lru.peek(key).cloned())
                .unwrap_or_default();
            let value = fill(&mut slot)?;

            let _mutations = self.lock_mutations();
            if generation == self.generation_for_locked(key, &_mutations) {
                let mut guard = self.lock_slots();
                let lru = guard
                    .as_mut()
                    .expect("inspection cache capacity invariant violated");
                let existed = lru.contains(key);
                let at_capacity = lru.len() >= self.capacity;
                lru.put(key.clone(), slot);
                if !existed && at_capacity {
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                }
            }
            return Ok(value);
        }
    }

    /// Returns summary data for a run, loading and caching on miss.
    pub fn get_summary(
        &self,
        key: &InspectionCacheKey,
        fingerprint: SummaryFingerprint,
        loader: impl FnOnce() -> Result<PersistedRunSummaryRecord>,
    ) -> Result<Arc<PersistedRunSummaryRecord>> {
        let mut loader = Some(loader);
        self.with_run_slot(
            key,
            |slot| {
                slot.summary.as_ref().and_then(|(fp, value)| {
                    if *fp == fingerprint {
                        Some(Arc::clone(value))
                    } else {
                        None
                    }
                })
            },
            |slot| {
                let load = loader
                    .take()
                    .ok_or_else(|| anyhow!("inspection cache loader called twice"))?;
                let value = Arc::new(load()?);
                slot.summary = Some((fingerprint, Arc::clone(&value)));
                Ok(value)
            },
        )
    }

    /// Returns run state for a run, loading and caching on miss.
    pub fn get_state(
        &self,
        key: &InspectionCacheKey,
        fingerprint: FileFingerprint,
        loader: impl FnOnce() -> Result<PersistedRunRecord>,
    ) -> Result<Arc<PersistedRunRecord>> {
        let mut loader = Some(loader);
        self.with_run_slot(
            key,
            |slot| {
                slot.state.as_ref().and_then(|(fp, value)| {
                    if *fp == fingerprint {
                        Some(Arc::clone(value))
                    } else {
                        None
                    }
                })
            },
            |slot| {
                let load = loader
                    .take()
                    .ok_or_else(|| anyhow!("inspection cache loader called twice"))?;
                let value = Arc::new(load()?);
                slot.state = Some((fingerprint, Arc::clone(&value)));
                Ok(value)
            },
        )
    }

    /// Returns parsed events for a run, loading and caching on miss.
    pub fn get_events(
        &self,
        key: &InspectionCacheKey,
        fingerprint: FileFingerprint,
        loader: impl FnOnce() -> Result<Vec<IndexedRuntimeEvent>>,
    ) -> Result<Arc<Vec<IndexedRuntimeEvent>>> {
        let mut loader = Some(loader);
        self.with_run_slot(
            key,
            |slot| {
                slot.events.as_ref().and_then(|entry| {
                    if entry.fingerprint == fingerprint {
                        Some(Arc::clone(&entry.parsed))
                    } else {
                        None
                    }
                })
            },
            |slot| {
                let load = loader
                    .take()
                    .ok_or_else(|| anyhow!("inspection cache loader called twice"))?;
                let value = Arc::new(load()?);
                slot.events = Some(EventsCacheEntry {
                    fingerprint,
                    parsed: Arc::clone(&value),
                });
                Ok(value)
            },
        )
    }

    /// Returns artifact/evidence listing for a run, loading and caching on miss.
    pub fn get_blob_listing(
        &self,
        key: &InspectionCacheKey,
        is_artifact: bool,
        fingerprint: FileFingerprint,
        loader: impl FnOnce() -> Result<Vec<IndexedBlob>>,
    ) -> Result<Arc<Vec<IndexedBlob>>> {
        let mut loader = Some(loader);
        self.with_run_slot(
            key,
            |slot| {
                let entry = if is_artifact {
                    slot.artifact_listing.as_ref()
                } else {
                    slot.evidence_listing.as_ref()
                }?;
                if entry.fingerprint == fingerprint {
                    Some(Arc::clone(&entry.blobs))
                } else {
                    None
                }
            },
            |slot| {
                let load = loader
                    .take()
                    .ok_or_else(|| anyhow!("inspection cache loader called twice"))?;
                let value = Arc::new(load()?);
                let entry = BlobListingCacheEntry {
                    fingerprint,
                    blobs: Arc::clone(&value),
                };
                if is_artifact {
                    slot.artifact_listing = Some(entry);
                } else {
                    slot.evidence_listing = Some(entry);
                }
                Ok(value)
            },
        )
    }

    /// Returns cached run ids currently held in the LRU.
    pub fn list_cached_run_ids(&self) -> Vec<String> {
        let mut guard = self.lock_slots();
        let Some(lru) = guard.as_mut() else {
            return Vec::new();
        };
        let mut ids: Vec<String> = lru.iter().map(|(key, _)| key.run_id.clone()).collect();
        ids.sort();
        ids
    }

    fn lock_slots(&self) -> MutexGuard<'_, Option<LruCache<InspectionCacheKey, RunCacheSlot>>> {
        match self.slots.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                let mut guard = poisoned.into_inner();
                *guard = NonZeroUsize::new(self.capacity).map(LruCache::new);
                self.slots.clear_poison();
                guard
            }
        }
    }

    fn lock_in_flight(&self) -> Result<MutexGuard<'_, HashSet<InspectionCacheKey>>> {
        match self.in_flight.0.lock() {
            Ok(guard) => Ok(guard),
            Err(_) => Err(anyhow!("inspection cache in-flight state is poisoned")),
        }
    }

    fn lock_mutations(&self) -> MutexGuard<'_, CacheMutations> {
        match self.mutations.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                let mut guard = poisoned.into_inner();
                *guard = CacheMutations::default();
                self.mutations.clear_poison();
                guard
            }
        }
    }

    fn generation_for(&self, key: &InspectionCacheKey) -> (u64, u64) {
        let mutations = self.lock_mutations();
        self.generation_for_locked(key, &mutations)
    }

    fn generation_for_locked(
        &self,
        key: &InspectionCacheKey,
        mutations: &CacheMutations,
    ) -> (u64, u64) {
        (
            mutations.clear_epoch,
            *mutations.key_generations.get(key).unwrap_or(&0),
        )
    }
}

#[derive(Debug, Default)]
struct CacheMutations {
    clear_epoch: u64,
    key_generations: HashMap<InspectionCacheKey, u64>,
}

struct InFlightGuard<'a> {
    cache: &'a InspectionCache,
    key: InspectionCacheKey,
}

impl<'a> InFlightGuard<'a> {
    fn new(cache: &'a InspectionCache, key: InspectionCacheKey) -> Self {
        Self { cache, key }
    }
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut in_flight) = self.cache.lock_in_flight() {
            in_flight.remove(&self.key);
            self.cache.in_flight.1.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::Duration;
    use transist_core::{KernelRunSnapshot, PersistedRunRecord};

    fn sample_record(run_id: &str) -> PersistedRunRecord {
        PersistedRunRecord::from_snapshot(
            KernelRunSnapshot::new(run_id, "wf", "v1", &["step".to_string()]),
            None,
            None,
            BTreeMap::new(),
            0,
            1,
            1,
        )
    }

    fn test_key(run_id: &str) -> InspectionCacheKey {
        InspectionCacheKey::new(Path::new("/"), run_id).unwrap()
    }

    #[test]
    fn lru_evicts_oldest_when_over_capacity() {
        let cache = InspectionCache::new(1);
        let fp = FileFingerprint {
            mtime_nanos: 1,
            len: 1,
        };
        let _ = cache
            .get_state(&test_key("run-a"), fp, || Ok(sample_record("run-a")))
            .unwrap();
        let _ = cache
            .get_state(&test_key("run-b"), fp, || Ok(sample_record("run-b")))
            .unwrap();

        let ids = cache.list_cached_run_ids();
        assert_eq!(ids, vec!["run-b".to_string()]);
        assert_eq!(cache.stats().evictions, 1);
    }

    #[test]
    fn capacity_zero_disables_cache() {
        let cache = InspectionCache::new(0);
        let calls = AtomicUsize::new(0);
        let fp = FileFingerprint {
            mtime_nanos: 1,
            len: 1,
        };
        let _ = cache
            .get_state(&test_key("run-a"), fp, || {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(sample_record("run-a"))
            })
            .unwrap();
        let _ = cache
            .get_state(&test_key("run-a"), fp, || {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(sample_record("run-a"))
            })
            .unwrap();

        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert!(cache.list_cached_run_ids().is_empty());
    }

    #[test]
    fn same_run_misses_are_single_flight() {
        let cache = Arc::new(InspectionCache::new(10));
        let calls = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(5));
        let handles = (0..4)
            .map(|_| {
                let cache = Arc::clone(&cache);
                let calls = Arc::clone(&calls);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    cache
                        .get_state(
                            &test_key("run-a"),
                            FileFingerprint {
                                mtime_nanos: 1,
                                len: 1,
                            },
                            || {
                                calls.fetch_add(1, Ordering::Relaxed);
                                thread::sleep(Duration::from_millis(20));
                                Ok(sample_record("run-a"))
                            },
                        )
                        .unwrap();
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn blocked_run_a_does_not_block_run_b() {
        let cache = Arc::new(InspectionCache::new(2));
        let started = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let worker_cache = Arc::clone(&cache);
        let worker_started = Arc::clone(&started);
        let worker_release = Arc::clone(&release);
        let worker = thread::spawn(move || {
            worker_cache
                .get_state(
                    &test_key("run-a"),
                    FileFingerprint {
                        mtime_nanos: 1,
                        len: 1,
                    },
                    || {
                        worker_started.wait();
                        worker_release.wait();
                        Ok(sample_record("run-a"))
                    },
                )
                .unwrap();
        });
        started.wait();

        let (run_b_done, run_b_seen) = std::sync::mpsc::channel();
        let run_b_cache = Arc::clone(&cache);
        let run_b = thread::spawn(move || {
            run_b_cache
                .get_state(
                    &test_key("run-b"),
                    FileFingerprint {
                        mtime_nanos: 1,
                        len: 1,
                    },
                    || {
                        run_b_done.send(()).unwrap();
                        Ok(sample_record("run-b"))
                    },
                )
                .unwrap();
        });

        assert!(run_b_seen.recv_timeout(Duration::from_secs(1)).is_ok());
        release.wait();
        run_b.join().unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn poisoned_cache_is_discarded_and_reusable() {
        let cache = Arc::new(InspectionCache::new(2));
        let poisoned = Arc::clone(&cache);
        let _ = thread::spawn(move || {
            let _guard = poisoned.slots.lock().unwrap();
            panic!("poison inspection cache");
        })
        .join();

        cache
            .get_state(
                &test_key("run-a"),
                FileFingerprint {
                    mtime_nanos: 1,
                    len: 1,
                },
                || Ok(sample_record("run-a")),
            )
            .unwrap();
        assert_eq!(cache.list_cached_run_ids(), vec!["run-a".to_string()]);
    }

    #[test]
    fn identical_run_ids_in_different_homes_do_not_collide() {
        let cache = InspectionCache::new(10);
        let fp = FileFingerprint {
            mtime_nanos: 1,
            len: 1,
        };
        let home_a = InspectionCacheKey {
            home_dir: PathBuf::from("/home-a"),
            run_id: "run-a".to_string(),
        };
        let home_b = InspectionCacheKey {
            home_dir: PathBuf::from("/home-b"),
            run_id: "run-a".to_string(),
        };

        let first = cache
            .get_state(&home_a, fp, || Ok(sample_record("home-a")))
            .unwrap();
        let second = cache
            .get_state(&home_b, fp, || Ok(sample_record("home-b")))
            .unwrap();

        assert_eq!(first.run_id, "home-a");
        assert_eq!(second.run_id, "home-b");
        assert_eq!(cache.stats().misses, 2);
    }

    #[test]
    fn invalidation_during_fill_does_not_reinsert_stale_data() {
        let cache = Arc::new(InspectionCache::new(2));
        let key = test_key("run-a");
        let started = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let worker_cache = Arc::clone(&cache);
        let worker_started = Arc::clone(&started);
        let worker_release = Arc::clone(&release);
        let worker_key = key.clone();
        let worker = thread::spawn(move || {
            worker_cache
                .get_state(
                    &worker_key,
                    FileFingerprint {
                        mtime_nanos: 1,
                        len: 1,
                    },
                    || {
                        worker_started.wait();
                        worker_release.wait();
                        Ok(sample_record("run-a"))
                    },
                )
                .unwrap();
        });

        started.wait();
        cache.invalidate(&key);
        release.wait();
        worker.join().unwrap();

        let calls = AtomicUsize::new(0);
        cache
            .get_state(
                &key,
                FileFingerprint {
                    mtime_nanos: 1,
                    len: 1,
                },
                || {
                    calls.fetch_add(1, Ordering::Relaxed);
                    Ok(sample_record("run-a"))
                },
            )
            .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }
}
