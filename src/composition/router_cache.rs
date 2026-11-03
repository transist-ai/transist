// SPDX-License-Identifier: MPL-2.0
use anyhow::{Context, Result};
use lru::LruCache;
use std::fmt;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use transist_adapters::AdapterRouter;
use transist_core::ports::ActiveRecordPort;

const ROUTER_CACHE_CAPACITY: usize = 32;

pub(crate) struct RouterCache {
    entries: Mutex<LruCache<PathBuf, Arc<AdapterRouter>>>,
    active_record_store: Arc<dyn ActiveRecordPort>,
}

impl fmt::Debug for RouterCache {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RouterCache")
            .finish_non_exhaustive()
    }
}

impl RouterCache {
    pub(crate) fn new(active_record_store: Arc<dyn ActiveRecordPort>) -> Self {
        Self {
            entries: Mutex::new(Self::new_entries()),
            active_record_store,
        }
    }

    pub(crate) fn local() -> Self {
        Self::new(Arc::new(transist_storage_local::LocalActiveRecordStore))
    }

    pub(crate) fn get_or_insert(&self, home_dir: &Path) -> Result<Arc<AdapterRouter>> {
        let key = canonical_cache_key(home_dir)?;
        let mut entries = self.lock_entries();
        if let Some(router) = entries.get(&key) {
            return Ok(Arc::clone(router));
        }

        let router = Arc::new(AdapterRouter::with_builtin_adapters(Arc::clone(
            &self.active_record_store,
        ))?);
        entries.put(key, Arc::clone(&router));
        Ok(router)
    }

    fn new_entries() -> LruCache<PathBuf, Arc<AdapterRouter>> {
        LruCache::new(
            NonZeroUsize::new(ROUTER_CACHE_CAPACITY).expect("non-zero router cache capacity"),
        )
    }

    fn lock_entries(&self) -> MutexGuard<'_, LruCache<PathBuf, Arc<AdapterRouter>>> {
        match self.entries.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                let mut guard = poisoned.into_inner();
                *guard = Self::new_entries();
                self.entries.clear_poison();
                guard
            }
        }
    }
}

/// Produces the canonical absolute key used for a router-cache home directory.
pub(crate) fn canonical_cache_key(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed resolving current directory for router cache key")?
            .join(path)
    };
    absolute
        .canonicalize()
        .with_context(|| format!("failed canonicalizing router home: {}", absolute.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use transist_storage_local::LocalActiveRecordStore;

    #[test]
    fn lexical_aliases_share_a_cache_key() {
        let home = tempfile::tempdir().expect("home");
        let path = home.path().join(".");
        assert_eq!(
            canonical_cache_key(&path).unwrap(),
            canonical_cache_key(home.path()).unwrap()
        );
    }

    #[test]
    fn canonicalization_failure_is_visible() {
        let home = tempfile::tempdir().expect("home");
        assert!(canonical_cache_key(&home.path().join("missing/transist-router-cache")).is_err());
    }

    #[test]
    fn router_is_retained_for_a_canonical_home_alias() {
        let cache = RouterCache::new(Arc::new(LocalActiveRecordStore));
        let home = tempfile::tempdir().expect("home");
        let first = cache.get_or_insert(home.path()).unwrap();
        let alias = home
            .path()
            .parent()
            .expect("temp home parent")
            .join(".")
            .join(home.path().file_name().expect("temp home name"));
        let second = cache.get_or_insert(&alias).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn distinct_cache_owned_stores_do_not_share_a_router() {
        let first_cache = RouterCache::new(Arc::new(LocalActiveRecordStore));
        let second_cache = RouterCache::new(Arc::new(LocalActiveRecordStore));
        let home = tempfile::tempdir().expect("home");

        let first = first_cache.get_or_insert(home.path()).unwrap();
        let second = second_cache.get_or_insert(home.path()).unwrap();

        assert!(!Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn cache_evicts_the_least_recently_used_router() {
        let cache = RouterCache::new(Arc::new(LocalActiveRecordStore));
        let home = tempfile::tempdir().expect("home");
        let first = cache.get_or_insert(home.path()).unwrap();
        let second_path = home.path().join("home-1");
        std::fs::create_dir(&second_path).expect("second cache home");
        let second = cache.get_or_insert(&second_path).unwrap();
        let first_weak = Arc::downgrade(&first);
        let second_weak = Arc::downgrade(&second);
        drop(first);
        drop(second);

        for index in 2..ROUTER_CACHE_CAPACITY {
            let path = home.path().join(format!("home-{index}"));
            std::fs::create_dir(&path).expect("cache home");
            cache.get_or_insert(&path).unwrap();
        }

        cache.get_or_insert(home.path()).unwrap();
        let last_path = home.path().join(format!("home-{ROUTER_CACHE_CAPACITY}"));
        std::fs::create_dir(&last_path).expect("last cache home");
        cache.get_or_insert(&last_path).unwrap();

        assert!(first_weak.upgrade().is_some());
        assert!(second_weak.upgrade().is_none());
    }

    #[test]
    fn poisoned_cache_recovers_as_an_empty_cache() {
        let cache = RouterCache::new(Arc::new(LocalActiveRecordStore));
        let home = tempfile::tempdir().expect("home");
        let first = cache.get_or_insert(home.path()).unwrap();
        let first_weak = Arc::downgrade(&first);
        drop(first);

        let cache = Arc::new(cache);
        let thread_cache = Arc::clone(&cache);
        std::thread::spawn(move || {
            let _entries = thread_cache.entries.lock().expect("entries");
            panic!("poison router cache");
        })
        .join()
        .expect_err("cache mutex should be poisoned");

        let replacement = cache.get_or_insert(home.path()).unwrap();
        assert!(first_weak.upgrade().is_none());
        assert!(!replacement.registered_kinds().is_empty());
    }

    #[test]
    fn concurrent_access_shares_one_router() {
        let cache = Arc::new(RouterCache::new(Arc::new(LocalActiveRecordStore)));
        let home = tempfile::tempdir().expect("home");
        let home_path = home.path().to_path_buf();
        let barrier = Arc::new(Barrier::new(16));

        std::thread::scope(|scope| {
            let routers = (0..16)
                .map(|_| {
                    let cache = Arc::clone(&cache);
                    let barrier = Arc::clone(&barrier);
                    let home_path = home_path.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        cache.get_or_insert(&home_path).expect("router")
                    })
                })
                .collect::<Vec<_>>();
            let mut routers = routers.into_iter();
            let first = routers
                .next()
                .expect("router thread")
                .join()
                .expect("router");
            for router in routers {
                assert!(Arc::ptr_eq(&first, &router.join().expect("router thread")));
            }
        });
    }

    #[test]
    fn dropping_cache_drops_cached_routers() {
        let home = tempfile::tempdir().expect("home");
        let weak = {
            let cache = RouterCache::new(Arc::new(LocalActiveRecordStore));
            let router = cache.get_or_insert(home.path()).unwrap();
            Arc::downgrade(&router)
        };

        assert!(weak.upgrade().is_none());
    }
}
