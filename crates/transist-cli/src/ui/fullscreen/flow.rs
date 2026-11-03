// SPDX-License-Identifier: MPL-2.0
//! Multi-step flow primitives shared by every fullscreen action that chains
//! more than one modal before committing.
//!
//! The TUI's single-modal path (text input, select, confirm, picker) returns
//! `Option<T>` where `Some(value)` is "Enter submitted" and `None` is
//! "Esc/Ctrl-C dismissed". That binary outcome forces the caller to discard
//! every prior step's data the moment a user wants to revise an earlier
//! answer.
//!
//! [`StepOutcome`] adds a third variant — [`StepOutcome::Back`] — so a
//! user pressing Esc inside a multi-step flow can pop one step instead of
//! aborting the entire flow. Ctrl-C remains the unconditional abort
//! (`StepOutcome::Cancel`). Step 1 of every flow is responsible for
//! converting a `Back` request from the user into a confirm-then-cancel
//! gesture (there is nothing to go back to), and that policy is encoded by
//! [`confirm_cancel_with_data_loss`].
//!
//! Adoption notes for new flows:
//! - Each flow decides locally how to display "Esc back" vs "Esc cancel" by
//!   passing the right [`StepHint`] when calling a `*_with_back` modal.
//! - The footer hint string is emitted via [`back_aware_help`] so all
//!   affected modals look identical.
//! - Existing single-modal call sites (`run_text_input_modal`,
//!   `run_select_modal`, `run_confirm_modal`, `pick_string_item`,
//!   `pick_launchable_workflow`) are intentionally untouched; they keep
//!   their original `Option<T>` API and Esc-equals-cancel semantics.

use anyhow::Result;

use super::prompt::FullscreenPrompt;

/// Outcome of a single step inside a multi-step TUI flow.
///
/// - `Ok(value)` — the user submitted (Enter / Ctrl-S / shortcut) and the
///   step yielded a value the caller should consume.
/// - `Back` — the user requested to return to the previous step (Esc inside
///   a multi-step flow). Step 1 must translate this into a confirm-then-cancel
///   gesture via [`confirm_cancel_with_data_loss`]; later steps simply pop
///   one level.
/// - `Cancel` — the user requested to abort the entire flow (Ctrl-C). The
///   caller should unwind to the surrounding screen with a status message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum StepOutcome<T> {
    Ok(T),
    Back,
    Cancel,
}

impl<T> StepOutcome<T> {
    #[allow(dead_code)]
    pub(super) fn map<U, F: FnOnce(T) -> U>(self, f: F) -> StepOutcome<U> {
        match self {
            StepOutcome::Ok(value) => StepOutcome::Ok(f(value)),
            StepOutcome::Back => StepOutcome::Back,
            StepOutcome::Cancel => StepOutcome::Cancel,
        }
    }
}

/// Hint context for a step's footer line. Flows tell each `*_with_back`
/// modal which position they're rendering in so the modal can advertise
/// the right Esc semantics: the very first step has nothing to go back to,
/// so we say "Esc cancel" there; later steps say "Esc back  Ctrl-C cancel".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StepHint {
    /// First step of a multi-step flow. Esc cancels (with confirm if data
    /// has been entered).
    First,
    /// Step 2+. Esc returns to the prior step preserving entered data;
    /// Ctrl-C aborts the whole flow.
    Subsequent,
}

/// Build the standard help/footer string used by every back-aware modal.
/// The submit description (e.g. `"Enter submit"` or
/// `"Ctrl+S submit"`) is supplied by the caller so the same helper works
/// for single-line text inputs, multi-line text inputs, and pickers.
///
/// Every text-input modal also accepts `Shift+Enter` *or* `Alt+Enter` to
/// insert a newline (so users can compose multi-line `--reason`, query,
/// and path values even when plain Enter submits). Both modifiers are
/// honored because terminal emulators disagree about which Enter variant
/// they forward; the footer advertises Shift+Enter as the primary
/// shortcut and notes Alt+Enter as a fallback. The hint is included
/// unconditionally because every current caller is a text-input flow;
/// if a future picker call site reuses this helper, it should advertise
/// its own footer instead.
pub(super) fn back_aware_help(submit_description: &str, hint: StepHint) -> String {
    match hint {
        StepHint::First => format!(
            "{}  Shift+Enter newline (also Alt+Enter)  Esc cancel",
            submit_description
        ),
        StepHint::Subsequent => format!(
            "{}  Shift+Enter newline (also Alt+Enter)  Esc back  Ctrl+C cancel",
            submit_description
        ),
    }
}

/// Render a "Discard entered data and cancel?" confirm modal. Returns
/// `true` when the user agrees to discard, `false` when they want to keep
/// editing. Defaults to No so an accidental Esc never blows away typed
/// content. Call this from a step-1 [`StepOutcome::Back`] branch when any
/// data has actually been entered; if no data is on the line yet, callers
/// should cancel directly without the confirm.
pub(super) fn confirm_cancel_with_data_loss(
    prompt: &mut FullscreenPrompt,
    message: &str,
) -> Result<bool> {
    prompt.run_confirm_modal("Discard and cancel?", message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn back_aware_help_first_step_says_cancel() {
        assert_eq!(
            back_aware_help("Enter submit", StepHint::First),
            "Enter submit  Shift+Enter newline (also Alt+Enter)  Esc cancel"
        );
    }

    #[test]
    fn back_aware_help_subsequent_step_says_back() {
        assert_eq!(
            back_aware_help("Enter submit", StepHint::Subsequent),
            "Enter submit  Shift+Enter newline (also Alt+Enter)  Esc back  Ctrl+C cancel"
        );
    }

    #[test]
    fn step_outcome_map_preserves_back_and_cancel() {
        let ok: StepOutcome<i32> = StepOutcome::Ok(7);
        assert_eq!(ok.map(|v| v + 1), StepOutcome::Ok(8));

        let back: StepOutcome<i32> = StepOutcome::Back;
        assert_eq!(back.map(|v| v + 1), StepOutcome::Back);

        let cancel: StepOutcome<i32> = StepOutcome::Cancel;
        assert_eq!(cancel.map(|v| v + 1), StepOutcome::Cancel);
    }
}
