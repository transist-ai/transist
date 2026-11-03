// SPDX-License-Identifier: MPL-2.0
pub(crate) mod output;
pub(crate) mod progress;
pub(crate) mod spinner;
pub(crate) mod summary;

pub(crate) fn emit_lines(lines: &[String]) {
    for line in lines {
        eprintln!("{}", line);
    }
}
