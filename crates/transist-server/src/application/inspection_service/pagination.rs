// SPDX-License-Identifier: MPL-2.0
use crate::domain::api_models::Pagination;

pub(crate) fn pagination(total: usize, limit: usize, offset: usize) -> Pagination {
    Pagination {
        total,
        limit,
        offset,
        has_more: offset.saturating_add(limit) < total,
    }
}
