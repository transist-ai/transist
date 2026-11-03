// SPDX-License-Identifier: MPL-2.0
//! Shared protocol handshake helpers for adapter implementations and probes.

use crate::mcp_runtime::mcp_protocol_error;
use crate::*;
use std::collections::BTreeSet;

pub(crate) fn validate_handshake(
    manifest: &PluginManifest,
    request: &ExecutionRequest,
    action: &ActionDescriptor,
    response: &AppHandshakeResponse,
) -> Result<()> {
    if response.message_type != protocol::APP_HANDSHAKE_RESPONSE_TYPE_V1 {
        bail!("invalid APP handshake type '{}'", response.message_type);
    }
    if response.protocol_version != APP_PROTOCOL_VERSION_V1 {
        bail!("unsupported APP protocol '{}'", response.protocol_version);
    }
    if response.plugin.id != manifest.id || response.plugin.version != manifest.version {
        bail!("APP handshake plugin identity mismatch");
    }
    if response.adapter_kind != request.adapter_kind {
        bail!("APP handshake adapter kind mismatch");
    }
    if !response
        .capabilities
        .actions
        .iter()
        .any(|name| name == &action.name)
    {
        bail!("APP handshake does not advertise action '{}'", action.name);
    }
    Ok(())
}

pub(crate) fn perform_http_handshake(
    base_url: &str,
    ca_cert: Option<&Path>,
    request: &ExecutionRequest,
    manifest: &PluginManifest,
    action: &ActionDescriptor,
    policy: &HttpRequestPolicy,
) -> Result<AppHandshakeResponse> {
    let handshake_value = http_json_request(
        "GET",
        base_url,
        "/app/handshake",
        None,
        &[],
        HttpRequestOptions {
            ca_cert,
            timeout_ms: request.timeout_ms,
            policy,
        },
    )
    .map_err(|error| anyhow!("http handshake transport failure: {error}"))?;
    let handshake: AppHandshakeResponse = serde_json::from_value(handshake_value)
        .map_err(|error| anyhow!("invalid HTTP handshake response: {error}"))?;
    validate_handshake(manifest, request, action, &handshake)?;
    Ok(handshake)
}

pub(crate) fn perform_http_health(
    base_url: &str,
    ca_cert: Option<&Path>,
    headers: &[(String, String)],
    timeout_ms: Option<u64>,
    policy: &HttpRequestPolicy,
) -> Result<()> {
    http_json_request(
        "GET",
        base_url,
        "/app/health",
        None,
        headers,
        HttpRequestOptions {
            ca_cert,
            timeout_ms,
            policy,
        },
    )
    .map(|_| ())
    .map_err(|error| anyhow!("http health probe failed: {error}"))
}

pub(crate) fn mcp_initialize_and_list_tools(
    stdin: &McpStdin,
    messages: &Receiver<Result<serde_json::Value>>,
    deadline: Instant,
) -> Result<Vec<String>> {
    let initialize_id = next_mcp_request_id();
    send_mcp_request(
        stdin,
        initialize_id,
        "initialize",
        serde_json::json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": "transist",
                "version": TRANSIST_ADAPTERS_VERSION,
            },
        }),
    )?;
    let initialize = recv_mcp_response(
        stdin,
        messages,
        deadline,
        initialize_id,
        "initialize response",
    )?;
    validate_mcp_initialize_response(&initialize)?;
    send_mcp_notification(stdin, "notifications/initialized", serde_json::json!({}))?;

    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    let mut seen_cursors = BTreeSet::new();
    loop {
        let mut params = serde_json::Map::new();
        if let Some(cursor) = cursor.as_ref() {
            params.insert(
                "cursor".to_string(),
                serde_json::Value::String(cursor.clone()),
            );
        }
        let request_id = next_mcp_request_id();
        send_mcp_request(
            stdin,
            request_id,
            "tools/list",
            serde_json::Value::Object(params),
        )?;
        let response =
            recv_mcp_response(stdin, messages, deadline, request_id, "tools/list response")?;
        let result = response
            .as_object()
            .ok_or_else(|| mcp_protocol_error("MCP tools/list result must be an object"))?;
        let page = result
            .get("tools")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| mcp_protocol_error("MCP tools/list result is missing tools array"))?;
        for tool in page {
            let name = tool
                .as_object()
                .and_then(|tool| tool.get("name"))
                .and_then(serde_json::Value::as_str)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| mcp_protocol_error("MCP tool is missing non-empty name"))?;
            tools.push(name.to_string());
        }
        cursor = match result.get("nextCursor") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(cursor)) if !cursor.is_empty() => Some(cursor.clone()),
            _ => {
                return Err(mcp_protocol_error(
                    "MCP tools/list nextCursor must be a non-empty string",
                ));
            }
        };
        if let Some(cursor) = cursor.as_ref()
            && !seen_cursors.insert(cursor.clone())
        {
            return Err(mcp_protocol_error("MCP tools/list cursor repeated"));
        }
        if cursor.is_none() {
            return Ok(tools);
        }
    }
}

fn validate_mcp_initialize_response(response: &serde_json::Value) -> Result<()> {
    let response = response
        .as_object()
        .ok_or_else(|| mcp_protocol_error("MCP initialize result must be an object"))?;
    if response
        .get("protocolVersion")
        .and_then(serde_json::Value::as_str)
        != Some(MCP_PROTOCOL_VERSION)
    {
        return Err(mcp_protocol_error(format!(
            "unsupported MCP protocol version; expected '{MCP_PROTOCOL_VERSION}'"
        )));
    }
    if !response
        .get("capabilities")
        .and_then(serde_json::Value::as_object)
        .and_then(|capabilities| capabilities.get("tools"))
        .is_some_and(serde_json::Value::is_object)
    {
        return Err(mcp_protocol_error(
            "MCP initialize result must declare tools capability",
        ));
    }
    Ok(())
}
