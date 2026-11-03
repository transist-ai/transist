// SPDX-License-Identifier: MPL-2.0
//! Opt-in structural work counts; no workflow data or package identity is retained.

use std::cell::RefCell;
use std::collections::BTreeMap;

/// Fixed operations counted by the test/benchmark observer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ValidationWork {
    /// Full authored definition semantic validation, including modules.
    DefinitionValidation,
    /// Full plugin manifest and schema contract validation.
    ContractValidation,
    /// Reading and parsing one package manifest.
    PackageLoad,
    /// Hashing one selected package payload.
    PackageHash,
    /// Compiling one schema root and its transitive local references.
    SchemaCompilation,
    /// Validating one materialized action input.
    InputValidation,
    /// Binding the current workflow inputs.
    InputBinding,
    /// Loading and evaluating current effective authority.
    PolicyEvaluation,
}

thread_local! {
    static COUNTS: RefCell<Option<BTreeMap<ValidationWork, usize>>> = const { RefCell::new(None) };
}

struct Guard;
impl Drop for Guard {
    fn drop(&mut self) {
        COUNTS.with(|counts| {
            counts.borrow_mut().take();
        });
    }
}

/// Captures fixed work counts on the current thread for one operation.
/// Returns the operation result and counts; nested captures panic. Nothing is
/// persisted or emitted, and unwind clears the observer.
pub fn capture_validation_work<T>(
    operation: impl FnOnce() -> T,
) -> (T, BTreeMap<ValidationWork, usize>) {
    COUNTS.with(|counts| {
        let mut counts = counts.borrow_mut();
        assert!(counts.is_none(), "validation work capture cannot be nested");
        *counts = Some(BTreeMap::new());
    });
    let guard = Guard;
    let result = operation();
    let counts = COUNTS.with(|counts| counts.borrow_mut().take().expect("active capture"));
    drop(guard);
    (result, counts)
}

/// Counts one fixed operation if capture is active; otherwise stores nothing.
/// The argument cannot carry inputs, paths, secrets, or plugin output.
#[doc(hidden)]
pub fn record_validation_work(work: ValidationWork) {
    COUNTS.with(|counts| {
        if let Some(counts) = counts.borrow_mut().as_mut() {
            *counts.entry(work).or_default() += 1;
        }
    });
}
