// SPDX-License-Identifier: MPL-2.0
use mime_guess::MimeGuess;

#[derive(Debug, Clone)]
/// Binary content response returned for artifact and evidence downloads.
pub struct BlobContentResponse {
    /// Suggested file name for the downloaded payload.
    pub file_name: String,
    /// Best-effort media type inferred from the stored relative path.
    pub content_type: String,
    /// Raw blob bytes read from local inspection storage.
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
/// Inline text-like content response used for workflow source and recipe views.
pub struct InlineContentResponse {
    /// Response content type selected from the referenced file path.
    pub content_type: String,
    /// Raw bytes returned to the caller.
    pub bytes: Vec<u8>,
}

pub(crate) fn map_blob_content_response(
    relative_path: String,
    file_name: String,
    bytes: Vec<u8>,
) -> BlobContentResponse {
    BlobContentResponse {
        file_name,
        content_type: MimeGuess::from_path(relative_path)
            .first_raw()
            .unwrap_or("application/octet-stream")
            .to_string(),
        bytes,
    }
}

pub(crate) fn map_inline_content_response(path: String, bytes: Vec<u8>) -> InlineContentResponse {
    InlineContentResponse {
        content_type: inline_content_type_for_path(&path).to_string(),
        bytes,
    }
}

fn inline_content_type_for_path(path: &str) -> &'static str {
    match std::path::Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("yaml" | "yml") => "text/yaml; charset=utf-8",
        Some("json") => "application/json",
        Some("txt" | "log" | "md" | "csv" | "html" | "htm" | "svg") => "text/plain; charset=utf-8",
        _ => "text/plain; charset=utf-8",
    }
}

pub(crate) fn media_type_for_path(path: &str) -> Option<String> {
    MimeGuess::from_path(path).first_raw().map(str::to_string)
}
