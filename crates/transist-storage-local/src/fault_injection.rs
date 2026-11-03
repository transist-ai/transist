// SPDX-License-Identifier: MPL-2.0
//! Test-only durable-operation fault controls for local storage.

use anyhow::{Result, bail};
use std::cell::Cell;
use std::cell::RefCell;
use std::path::{Path, PathBuf};

thread_local! {
    static INJECTED_POINT: Cell<(usize, usize, usize)> = const { Cell::new((0, 0, 0)) };
    static PUBLICATION_PAUSE: RefCell<Option<(PathBuf, PathBuf)>> = const { RefCell::new(None) };
    #[cfg(test)]
    static AFTER_TEMP_CREATE_HOOK: RefCell<Option<Box<dyn FnOnce() + 'static>>> =
        const { RefCell::new(None) };
    #[cfg(test)]
    static TREE_CAPTURE_HOOK: RefCell<Option<TreeCaptureHook>> = const { RefCell::new(None) };
}

/// Arms a subprocess-only pause after the exact target's durable hard-link
/// publication and before temp cleanup. Writes `ready` then parks until killed;
/// unlike an injected error, this seam never unwinds or removes the temp alias.
pub fn pause_after_durable_publication(target: &Path, ready: &Path) {
    PUBLICATION_PAUSE.with(|pause| *pause.borrow_mut() = Some((target.into(), ready.into())));
}

pub(crate) fn run_durable_publication_pause(target: &Path) -> Result<()> {
    let ready = PUBLICATION_PAUSE.with(|pause| {
        pause
            .borrow()
            .as_ref()
            .filter(|(path, _)| path == target)
            .map(|(_, ready)| ready.clone())
    });
    if let Some(ready) = ready {
        std::fs::write(ready, b"durable publication before temp cleanup")?;
        loop {
            std::thread::park();
        }
    }
    Ok(())
}

#[cfg(test)]
type TreeCaptureHook = Box<dyn FnMut(&Path, bool)>;

#[cfg(test)]
pub(crate) struct TreeCaptureHookGuard;

#[cfg(test)]
impl Drop for TreeCaptureHookGuard {
    fn drop(&mut self) {
        TREE_CAPTURE_HOOK.with(|hook| *hook.borrow_mut() = None);
    }
}

#[cfg(test)]
pub(crate) fn after_tree_capture_hook(
    hook: impl FnMut(&Path, bool) + 'static,
) -> TreeCaptureHookGuard {
    TREE_CAPTURE_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    TreeCaptureHookGuard
}

pub(crate) fn run_tree_capture_hook(path: &Path, directory: bool) {
    #[cfg(test)]
    TREE_CAPTURE_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(path, directory);
        }
    });
    #[cfg(not(test))]
    let _ = (path, directory);
}

/// Fault before creating a new durable file.
pub const FAULT_WRITE_NEW_FILE: usize = 101;
/// Fault before creating a replacement temp file.
pub const FAULT_WRITE_REPLACE_FILE: usize = 102;
/// Fault before opening a JSONL append stream.
pub const FAULT_APPEND_JSONL: usize = 103;
/// Fault after a durable file write.
pub const FAULT_AFTER_WRITE: usize = 110;
/// Fault after a file flush.
pub const FAULT_AFTER_FLUSH: usize = 111;
/// Fault after a file sync.
pub const FAULT_AFTER_FILE_SYNC: usize = 112;
/// Fault after a rename.
pub const FAULT_AFTER_RENAME: usize = 113;
/// Fault after a hard link.
pub const FAULT_AFTER_LINK: usize = 114;
/// Fault after a directory sync.
pub const FAULT_AFTER_DIRECTORY_SYNC: usize = 115;
/// Fault after removing a file and before syncing its parent.
pub const FAULT_AFTER_REMOVE_FILE: usize = 116;
/// Fault after staging `state.json`.
pub const FAULT_RUN_BIRTH_AFTER_STATE: usize = 201;
/// Fault after staging `recipe.yaml`.
pub const FAULT_RUN_BIRTH_AFTER_RECIPE: usize = 202;
/// Fault after staging run inputs.
pub const FAULT_RUN_BIRTH_AFTER_INPUTS: usize = 203;
/// Fault after staging provenance.
pub const FAULT_RUN_BIRTH_AFTER_PROVENANCE: usize = 204;
/// Fault before renaming the run-birth staging directory.
pub const FAULT_RUN_BIRTH_BEFORE_RENAME: usize = 205;
/// Fault after renaming the run-birth staging directory.
pub const FAULT_RUN_BIRTH_AFTER_RENAME: usize = 206;
/// Fault after publishing a workflow recipe during installation.
pub const FAULT_WORKFLOW_INSTALL_AFTER_RECIPE: usize = 301;
/// Fault after publishing workflow trust metadata during installation.
pub const FAULT_WORKFLOW_INSTALL_AFTER_TRUST: usize = 302;

/// Arms one durable-operation fault point.
/// Fault before storing candidate document blobs.
pub const FAULT_DOCUMENT_BEFORE_BLOBS: usize = 401;
/// Fault after storing all candidate blobs, before candidate metadata.
pub const FAULT_DOCUMENT_AFTER_BLOBS: usize = 402;
/// Fault before the immutable completion publication.
pub const FAULT_DOCUMENT_BEFORE_COMMIT: usize = 403;
/// Fault after completion publication, before the heads projection.
pub const FAULT_DOCUMENT_AFTER_COMMIT: usize = 404;
/// Fault before atomic heads replacement.
pub const FAULT_DOCUMENT_BEFORE_HEADS: usize = 405;
/// Fault after atomic heads replacement.
pub const FAULT_DOCUMENT_AFTER_HEADS: usize = 406;
/// Fault after one content-addressed document blob is durable.
pub const FAULT_DOCUMENT_AFTER_BLOB: usize = 407;

/// Arms one durable-operation fault point.
/// Arms one durable-operation fault point.
pub fn set(point: usize) {
    set_occurrence(point, 1);
}

/// Arms one durable-operation fault point for a selected occurrence.
/// Arms one durable-operation fault point for a selected occurrence.
pub fn set_occurrence(point: usize, occurrence: usize) {
    INJECTED_POINT.with(|injected| injected.set((point, occurrence.max(1), 0)));
}

/// Clears the current fault point.
/// Clears the current fault point.
pub fn clear() {
    INJECTED_POINT.with(|injected| injected.set((0, 0, 0)));
}

/// Returns the currently armed fault point, or zero.
/// Returns the currently armed fault point, or zero.
pub fn armed() -> usize {
    INJECTED_POINT.with(|injected| injected.get().0)
}

/// Returns whether `point` is the selected occurrence of the armed fault.
/// Returns whether `point` is the selected occurrence of the armed fault.
pub fn matches(point: usize) -> bool {
    INJECTED_POINT.with(|injected| {
        let (armed, target, seen) = injected.get();
        if armed != point || target == 0 {
            return false;
        }
        let next_seen = seen.saturating_add(1);
        injected.set((armed, target, next_seen));
        next_seen == target
    })
}

/// Returns an injected fault error when the selected point is reached.
pub fn check_fault(point: usize) -> Result<()> {
    if matches(point) {
        bail!("injected fault at durable-write point {point}");
    }
    Ok(())
}

/// Installs a test-only hook invoked between temp-file creation/sync and the
/// anchored publication (see `atomic_io` write paths). This lets a test
/// simulate a same-user racer that swaps the parent directory for a symlink in
/// the window the fix closes: the parent descriptor is already held, so the
/// publication must resolve through the held descriptor and ignore the swap.
///
/// The hook is a no-op when unset (production). Only one hook is kept per
/// thread; setting again replaces the previous one.
#[cfg(test)]
pub struct AfterTempCreateHookGuard;

#[cfg(test)]
impl Drop for AfterTempCreateHookGuard {
    fn drop(&mut self) {
        clear_after_temp_create_hook();
    }
}

/// Installs a one-shot test hook and clears it when the returned guard drops.
#[cfg(test)]
pub fn after_temp_create_hook(hook: impl FnOnce() + 'static) -> AfterTempCreateHookGuard {
    AFTER_TEMP_CREATE_HOOK.with(|cell| *cell.borrow_mut() = Some(Box::new(hook)));
    AfterTempCreateHookGuard
}

/// Removes the test-only after-temp-create hook.
#[cfg(test)]
fn clear_after_temp_create_hook() {
    AFTER_TEMP_CREATE_HOOK.with(|cell| *cell.borrow_mut() = None);
}

/// Runs the test-only after-temp-create hook if one is installed. Called by
/// the atomic write paths between temp sync and publication.
pub(crate) fn run_after_temp_create_hook() {
    #[cfg(test)]
    AFTER_TEMP_CREATE_HOOK.with(|cell| {
        // Take before invocation so hooks may safely install another hook.
        if let Some(hook) = cell.borrow_mut().take() {
            hook();
        }
    });
}
