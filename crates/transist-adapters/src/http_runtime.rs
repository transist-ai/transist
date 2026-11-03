// SPDX-License-Identifier: MPL-2.0
//! Shared HTTP transport helpers for invoke, cancel, and preflight calls.

use super::*;
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::redirect::Policy;
use reqwest::{Certificate, Method, StatusCode};
use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use url::Url;

const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_REDIRECT_LIMIT: usize = 10;

/// Carries the effective network policy and the action-specific secret paths
/// needed to safely follow an explicitly allowed cross-authority redirect.
#[derive(Debug, Clone)]
pub(crate) struct HttpRequestPolicy {
    network: NetworkPolicy,
    secret_headers: BTreeSet<String>,
    secret_body_pointers: BTreeSet<String>,
}

impl HttpRequestPolicy {
    /// Builds transport redirect protections from the effective policy and
    /// action-specific secret bindings.
    pub(crate) fn from_action(network: NetworkPolicy, action: &ActionDescriptor) -> Self {
        let mut secret_headers = BTreeSet::new();
        let mut secret_body_pointers = BTreeSet::new();
        for input in &action.secret_inputs {
            match &input.binding {
                SecretBinding::Header { name } => {
                    secret_headers.insert(name.to_ascii_lowercase());
                }
                SecretBinding::BodyField { name } => {
                    // HTTP body-field secrets are injected into the APP invoke
                    // envelope's `input` object by `secrets.rs` and
                    // `http_adapter.rs`.
                    secret_body_pointers.insert(format!("/input/{}", rfc6901_escape(name)));
                }
                SecretBinding::Env { .. }
                | SecretBinding::TempFile { .. }
                | SecretBinding::Arg { .. } => {}
            }
        }
        Self {
            network,
            secret_headers,
            secret_body_pointers,
        }
    }
}

/// Per-request transport options shared by every HTTP adapter phase.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HttpRequestOptions<'a> {
    pub(crate) ca_cert: Option<&'a Path>,
    pub(crate) timeout_ms: Option<u64>,
    pub(crate) policy: &'a HttpRequestPolicy,
}

fn parse_http_url(base_url: &str, endpoint: &str) -> Result<Url> {
    let url = Url::parse(&format!("{}{}", base_url.trim_end_matches('/'), endpoint))
        .with_context(|| format!("invalid HTTP URL '{}{}'", base_url, endpoint))?;
    validate_http_url(&url)
}

fn validate_http_url(url: &Url) -> Result<Url> {
    if !matches!(url.scheme(), "http" | "https") {
        bail!("HTTP URL must use http or https scheme");
    }
    if url.host_str().is_none() {
        bail!("HTTP URL must include a host");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("HTTP URL userinfo is not supported");
    }
    Ok(url.clone())
}

fn effective_port(url: &Url) -> Result<u16> {
    url.port_or_known_default()
        .ok_or_else(|| anyhow!("HTTP URL has no effective port"))
}

fn normalized_host(host: &str) -> String {
    host.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

fn allowlist_host_matches(url: &Url, port: u16, allow_host: &str) -> bool {
    let allow_host = allow_host.trim();
    if allow_host.contains("://") {
        let Ok(allowed_url) = Url::parse(allow_host) else {
            return false;
        };
        return matches!(allowed_url.scheme(), "http" | "https")
            && allowed_url.scheme() == url.scheme()
            && allowed_url.host_str().is_some_and(|host| {
                url.host_str()
                    .is_some_and(|actual| normalized_host(host) == normalized_host(actual))
            })
            && allowed_url.port_or_known_default() == Some(port);
    }
    url.host_str()
        .is_some_and(|host| normalized_host(host) == normalized_host(allow_host))
}

fn rfc6901_escape(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

fn rfc6901_unescape(token: &str) -> Option<String> {
    let mut result = String::with_capacity(token.len());
    let mut chars = token.chars();
    while let Some(character) = chars.next() {
        if character != '~' {
            result.push(character);
            continue;
        }
        match chars.next()? {
            '0' => result.push('~'),
            '1' => result.push('/'),
            _ => return None,
        }
    }
    Some(result)
}

fn remove_at_pointer(root: &mut serde_json::Value, pointer: &str) {
    let Some(last_separator) = pointer.rfind('/') else {
        return;
    };
    let parent_pointer = &pointer[..last_separator];
    let Some(field) = rfc6901_unescape(&pointer[last_separator + 1..]) else {
        return;
    };
    let parent = if parent_pointer.is_empty() {
        Some(root)
    } else {
        root.pointer_mut(parent_pointer)
    };
    if let Some(serde_json::Value::Object(object)) = parent {
        object.remove(&field);
    }
}

fn resolve_validated_destination(url: &Url, policy: &NetworkPolicy) -> Result<SocketAddr> {
    let url = validate_http_url(url)?;
    if policy.mode == NetworkAccessMode::None {
        bail!("HTTP network policy denied all connections (mode: none)");
    }
    let port = effective_port(&url)?;
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("HTTP URL must include a host"))?;
    let host_name = normalized_host(host);
    if policy.tls_required && url.scheme() != "https" {
        bail!(
            "HTTP network policy requires TLS for '{}://{}'",
            url.scheme(),
            host_name
        );
    }
    if policy.mode == NetworkAccessMode::Allowlist
        && (!policy.allow_ports.contains(&port)
            || !policy
                .allow_hosts
                .iter()
                .any(|allow_host| allowlist_host_matches(&url, port, allow_host)))
    {
        bail!(
            "HTTP network policy denied '{}://{}:{}' (not in allowlist)",
            url.scheme(),
            host_name,
            port
        );
    }

    let mut addresses = if let Ok(ip) = host.parse::<IpAddr>() {
        vec![SocketAddr::new(ip, port)]
    } else {
        (host, port)
            .to_socket_addrs()
            .with_context(|| format!("failed resolving HTTP host '{host}'"))?
            .collect()
    };
    if addresses.is_empty() {
        bail!("HTTP host '{}' resolved to no addresses", host_name);
    }
    if policy.mode == NetworkAccessMode::Loopback
        && addresses.iter().any(|address| !address.ip().is_loopback())
    {
        bail!(
            "HTTP network policy denied '{}://{}:{}': DNS result is not exclusively loopback",
            url.scheme(),
            host_name,
            port
        );
    }
    if policy.mode == NetworkAccessMode::Loopback
        && !policy.allow_ports.is_empty()
        && !policy.allow_ports.contains(&port)
    {
        bail!(
            "HTTP network policy denied '{}://{}:{}' (port not in authored allowlist)",
            url.scheme(),
            host_name,
            port
        );
    }
    if policy.mode == NetworkAccessMode::Loopback
        && !policy.allow_hosts.is_empty()
        && !policy
            .allow_hosts
            .iter()
            .any(|allow_host| allowlist_host_matches(&url, port, allow_host))
    {
        bail!(
            "HTTP network policy denied '{}://{}:{}' (host not in authored allowlist)",
            url.scheme(),
            host_name,
            port
        );
    }
    // Prefer IPv4 when both families resolve. This keeps dual-stack loopback
    // hosts such as `localhost` aligned with listeners bound to 127.0.0.1
    // while still validating every resolved address above.
    addresses.sort_by_key(SocketAddr::is_ipv6);
    Ok(addresses[0])
}

fn same_authority(left: &Url, right: &Url) -> Result<bool> {
    Ok(left.scheme() == right.scheme()
        && left.host_str().map(normalized_host) == right.host_str().map(normalized_host)
        && effective_port(left)? == effective_port(right)?)
}

fn build_http_client(
    url: &Url,
    ca_cert: Option<&Path>,
    policy: &HttpRequestPolicy,
) -> Result<Client> {
    let resolved = resolve_validated_destination(url, &policy.network)?;
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("HTTP URL must include a host"))?;
    let mut builder = Client::builder()
        .redirect(Policy::none())
        .gzip(true)
        .resolve(host, resolved);
    if !matches!(policy.network.mode, NetworkAccessMode::Any) {
        builder = builder.no_proxy();
    }
    if let Some(ca_cert_path) = ca_cert {
        let pem = fs::read(ca_cert_path).with_context(|| {
            format!("failed reading CA certificate '{}'", ca_cert_path.display())
        })?;
        let certificates = Certificate::from_pem_bundle(&pem)
            .with_context(|| format!("invalid CA certificate '{}'", ca_cert_path.display()))?;
        if certificates.is_empty() {
            bail!("invalid CA certificate '{}'", ca_cert_path.display());
        }
        for certificate in certificates {
            builder = builder.add_root_certificate(certificate);
        }
    }
    builder
        .build()
        .context("failed building reqwest HTTP client")
}

fn apply_common_headers(request: RequestBuilder, headers: &[(String, String)]) -> RequestBuilder {
    headers.iter().fold(
        request.header(reqwest::header::ACCEPT, "application/json"),
        |request, (key, value)| request.header(key, value),
    )
}

fn map_non_success_status(status: StatusCode, body: &str) -> anyhow::Error {
    anyhow!(
        "http transport failed with status {}: {}",
        status,
        body.trim()
    )
}

pub(crate) fn http_json_request(
    method: &str,
    base_url: &str,
    endpoint: &str,
    body: Option<serde_json::Value>,
    headers: &[(String, String)],
    options: HttpRequestOptions<'_>,
) -> Result<serde_json::Value> {
    let mut method = Method::from_bytes(method.as_bytes())
        .with_context(|| format!("unsupported HTTP method '{method}'"))?;
    let mut url = parse_http_url(base_url, endpoint)?;
    let mut body = body;
    let mut headers = headers.to_vec();
    let mut redirects = 0;
    let redirect_limit = if options.policy.network.max_redirects == 0 {
        DEFAULT_REDIRECT_LIMIT
    } else {
        options.policy.network.max_redirects as usize
    };

    loop {
        let client = build_http_client(&url, options.ca_cert, options.policy)?;
        let request = client
            .request(method.clone(), url.clone())
            .timeout(Duration::from_millis(
                options.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS),
            ));
        let request = apply_common_headers(request, &headers);
        let request = if let Some(request_body) = body.clone() {
            request.json(&request_body)
        } else {
            request
        };
        let response = request
            .send()
            .with_context(|| format!("HTTP request failed for {url}"))?;
        let status = response.status();
        if status.is_redirection() {
            let Some(location) = response.headers().get(reqwest::header::LOCATION) else {
                let response_body = response
                    .text()
                    .with_context(|| format!("failed reading HTTP response body for {url}"))?;
                return Err(map_non_success_status(status, &response_body));
            };
            if redirects >= redirect_limit {
                bail!("HTTP redirect limit exceeded ({redirect_limit})");
            }
            let location = location
                .to_str()
                .context("HTTP redirect location is not valid UTF-8")?;
            let next_url = url
                .join(location)
                .context("HTTP redirect location is invalid")?;
            validate_http_url(&next_url)?;
            if next_url.scheme() == "http" && url.scheme() == "https" {
                bail!("HTTP redirect would downgrade TLS");
            }
            let cross_authority = !same_authority(&url, &next_url)?;
            if cross_authority && options.policy.network.mode != NetworkAccessMode::Allowlist {
                let host = next_url
                    .host_str()
                    .ok_or_else(|| anyhow!("HTTP redirect target must include a host"))?;
                let display_host = if host.contains(':') {
                    format!("[{host}]")
                } else {
                    host.to_string()
                };
                bail!(
                    "HTTP redirect to '{}://{}:{}' denied; enumerate it under an allowlist network policy",
                    next_url.scheme(),
                    display_host,
                    effective_port(&next_url)?
                );
            }
            resolve_validated_destination(&next_url, &options.policy.network)?;
            if cross_authority {
                headers.retain(|(name, _)| {
                    !options
                        .policy
                        .secret_headers
                        .contains(&name.to_ascii_lowercase())
                });
                if let Some(request_body) = body.as_mut() {
                    for pointer in &options.policy.secret_body_pointers {
                        remove_at_pointer(request_body, pointer);
                    }
                }
            }
            if matches!(
                status,
                StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND | StatusCode::SEE_OTHER
            ) && method == Method::POST
            {
                method = Method::GET;
                body = None;
            }
            url = next_url;
            redirects += 1;
            continue;
        }
        let response_body = response
            .text()
            .with_context(|| format!("failed reading HTTP response body for {url}"))?;
        if !status.is_success() {
            return Err(map_non_success_status(status, &response_body));
        }
        return serde_json::from_str(&response_body).context("invalid HTTP JSON response");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::mpsc;

    fn read_request(socket: &mut TcpStream) -> String {
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = socket.read(&mut chunk).expect("read request chunk");
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(split) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&buffer[..split]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("Content-Length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if buffer.len() >= split + 4 + content_length {
                    break;
                }
            }
        }
        String::from_utf8(buffer).expect("request should be utf-8")
    }

    fn write_response(socket: &mut TcpStream, status: &str, body: &str, extra_headers: &[&str]) {
        let mut response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        for header in extra_headers {
            response.push_str(header);
            response.push_str("\r\n");
        }
        response.push_str("\r\n");
        response.push_str(body);
        socket
            .write_all(response.as_bytes())
            .expect("write response");
    }

    fn bind_server() -> (TcpListener, SocketAddr) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let addr = listener.local_addr().expect("resolve local addr");
        (listener, addr)
    }

    fn any_policy() -> HttpRequestPolicy {
        HttpRequestPolicy {
            network: NetworkPolicy::default(),
            secret_headers: BTreeSet::new(),
            secret_body_pointers: BTreeSet::new(),
        }
    }

    fn request_options<'a>(
        policy: &'a HttpRequestPolicy,
        ca_cert: Option<&'a Path>,
        timeout_ms: Option<u64>,
    ) -> HttpRequestOptions<'a> {
        HttpRequestOptions {
            ca_cert,
            timeout_ms,
            policy,
        }
    }

    fn network_policy(mode: NetworkAccessMode, hosts: &[&str], ports: &[u16]) -> HttpRequestPolicy {
        HttpRequestPolicy {
            network: NetworkPolicy {
                mode,
                allow_hosts: hosts.iter().map(|host| (*host).to_string()).collect(),
                allow_ports: ports.to_vec(),
                tls_required: false,
                max_redirects: 0,
            },
            secret_headers: BTreeSet::new(),
            secret_body_pointers: BTreeSet::new(),
        }
    }

    #[test]
    fn none_policy_rejects_loopback_before_connecting() {
        let (_listener, address) = bind_server();
        let policy = network_policy(NetworkAccessMode::None, &[], &[]);
        let error = http_json_request(
            "GET",
            &format!("http://{address}"),
            "/denied",
            None,
            &[],
            request_options(&policy, None, Some(1_000)),
        )
        .expect_err("none policy must deny loopback");
        assert!(error.to_string().contains("mode: none"));
    }

    #[test]
    fn loopback_policy_rejects_non_loopback_destination() {
        let policy = network_policy(NetworkAccessMode::Loopback, &[], &[]);
        let error = http_json_request(
            "GET",
            "http://example.com",
            "/denied",
            None,
            &[],
            request_options(&policy, None, Some(1_000)),
        )
        .expect_err("loopback policy must deny a public destination");
        assert!(error.to_string().contains("not exclusively loopback"));
    }

    #[test]
    fn loopback_mode_honors_allow_ports() {
        let (listener, address) = bind_server();
        listener
            .set_nonblocking(true)
            .expect("set nonblocking listener");
        let policy = network_policy(
            NetworkAccessMode::Loopback,
            &[],
            &[address.port().saturating_add(1)],
        );
        let error = http_json_request(
            "GET",
            &format!("http://{address}"),
            "/denied",
            None,
            &[],
            request_options(&policy, None, Some(1_000)),
        )
        .expect_err("loopback policy must enforce authored ports");
        assert!(error.to_string().contains("port not in authored allowlist"));
        assert!(matches!(
            listener.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
    }

    #[test]
    fn loopback_mode_honors_allow_hosts() {
        let (listener, address) = bind_server();
        listener
            .set_nonblocking(true)
            .expect("set nonblocking listener");
        let policy = network_policy(
            NetworkAccessMode::Loopback,
            &["localhost"],
            &[address.port()],
        );
        let error = http_json_request(
            "GET",
            &format!("http://{address}"),
            "/denied",
            None,
            &[],
            request_options(&policy, None, Some(1_000)),
        )
        .expect_err("loopback policy must enforce authored hosts");
        assert!(error.to_string().contains("host not in authored allowlist"));
        assert!(matches!(
            listener.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
    }

    #[test]
    fn allowlist_requires_normalized_host_and_effective_port() {
        let (_listener, address) = bind_server();
        let policy = network_policy(
            NetworkAccessMode::Allowlist,
            &["127.0.0.1."],
            &[address.port()],
        );
        let response = http_json_request(
            "GET",
            &format!("http://127.0.0.1:{}/", address.port()),
            "/denied",
            None,
            &[],
            request_options(&policy, None, Some(1_000)),
        );
        let error = response.expect_err("listener has no response");
        assert!(!error.to_string().contains("not in allowlist"));
    }

    #[test]
    fn allowed_cross_authority_redirect_strips_secret_headers_and_body_fields() {
        let (first_listener, first_address) = bind_server();
        let (second_listener, second_address) = bind_server();
        let handle = thread::spawn(move || {
            let (mut first, _) = first_listener.accept().expect("accept first redirect");
            let _ = read_request(&mut first);
            write_response(
                &mut first,
                "307 Temporary Redirect",
                "",
                &[&format!(
                    "Location: http://127.0.0.1:{}/final",
                    second_address.port()
                )],
            );
            let (mut second, _) = second_listener.accept().expect("accept final redirect");
            let request = read_request(&mut second);
            assert!(
                !request
                    .to_ascii_lowercase()
                    .contains("authorization: secret")
            );
            let body = request
                .split_once("\r\n\r\n")
                .map(|(_, body)| body)
                .unwrap_or("{}");
            let body: serde_json::Value = serde_json::from_str(body).expect("redirect body");
            assert!(body["input"].get("token").is_none());
            assert_eq!(body["input"]["keep"], serde_json::json!(true));
            write_response(&mut second, "200 OK", r#"{"redirected":true}"#, &[]);
        });
        let policy = HttpRequestPolicy {
            network: NetworkPolicy {
                mode: NetworkAccessMode::Allowlist,
                allow_hosts: vec!["127.0.0.1".to_string()],
                allow_ports: vec![first_address.port(), second_address.port()],
                tls_required: false,
                max_redirects: 2,
            },
            secret_headers: ["authorization".to_string()].into_iter().collect(),
            secret_body_pointers: ["/input/token".to_string()].into_iter().collect(),
        };
        let response = http_json_request(
            "POST",
            &format!("http://127.0.0.1:{}", first_address.port()),
            "/start",
            Some(serde_json::json!({"input":{"token":"secret","keep":true}})),
            &[("Authorization".to_string(), "secret".to_string())],
            request_options(&policy, None, Some(1_000)),
        )
        .expect("allowed redirect");
        assert_eq!(response["redirected"], serde_json::json!(true));
        handle.join().expect("redirect thread");
    }

    #[test]
    fn any_mode_denies_cross_authority_redirect_with_zero_connection() {
        let (first_listener, first_address) = bind_server();
        let (second_listener, second_address) = bind_server();
        second_listener
            .set_nonblocking(true)
            .expect("set nonblocking listener");
        let handle = thread::spawn(move || {
            let (mut first, _) = first_listener.accept().expect("accept first redirect");
            let _ = read_request(&mut first);
            write_response(
                &mut first,
                "307 Temporary Redirect",
                "",
                &[&format!(
                    "Location: http://127.0.0.1:{}/final",
                    second_address.port()
                )],
            );
        });
        let error = http_json_request(
            "GET",
            &format!("http://{first_address}"),
            "/start",
            None,
            &[],
            request_options(&any_policy(), None, Some(1_000)),
        )
        .expect_err("any mode must deny cross-authority redirects");
        assert!(
            error
                .to_string()
                .contains("enumerate it under an allowlist network policy")
        );
        assert!(matches!(
            second_listener.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
        handle.join().expect("redirect thread");
    }

    #[test]
    fn remove_at_pointer_supports_rfc6901_escaped_fields() {
        let mut body = serde_json::json!({
            "input": {
                "token/name~value": "secret",
                "keep": true
            }
        });
        remove_at_pointer(&mut body, "/input/token~1name~0value");
        assert!(body["input"].get("token/name~value").is_none());
        assert_eq!(body["input"]["keep"], serde_json::json!(true));
    }

    #[test]
    fn http_json_request_applies_timeout() {
        let (listener, addr) = bind_server();
        let (tx, rx) = mpsc::channel::<()>();
        let handle = thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept request");
            let _ = read_request(&mut socket);
            thread::sleep(Duration::from_millis(200));
            write_response(&mut socket, "200 OK", r#"{"ok":true}"#, &[]);
            tx.send(()).expect("notify done");
        });

        let result = http_json_request(
            "GET",
            &format!("http://{addr}"),
            "/slow",
            None,
            &[],
            request_options(&any_policy(), None, Some(50)),
        );
        assert!(result.is_err());
        let error = result.expect_err("timeout should fail").to_string();
        assert!(error.contains("HTTP request failed"));

        let _ = rx.recv_timeout(Duration::from_secs(1));
        handle.join().expect("join server thread");
    }

    #[test]
    fn http_json_request_follows_redirect_with_cap() {
        let (listener, addr) = bind_server();
        let handle = thread::spawn(move || {
            let (mut first, _) = listener.accept().expect("accept first");
            let first_request = read_request(&mut first);
            assert!(first_request.starts_with("GET /start"));
            write_response(
                &mut first,
                "302 Found",
                "",
                &["Location: /final", "Content-Length: 0"],
            );

            let (mut second, _) = listener.accept().expect("accept redirect target");
            let second_request = read_request(&mut second);
            assert!(second_request.starts_with("GET /final"));
            write_response(&mut second, "200 OK", r#"{"redirected":true}"#, &[]);
        });

        let response = http_json_request(
            "GET",
            &format!("http://{addr}"),
            "/start",
            None,
            &[],
            request_options(&any_policy(), None, Some(1_000)),
        )
        .expect("redirect should resolve");
        assert_eq!(response["redirected"], serde_json::json!(true));
        handle.join().expect("join server thread");
    }

    #[test]
    fn http_json_request_maps_non_success_status_to_error() {
        let (listener, addr) = bind_server();
        let handle = thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept request");
            let _ = read_request(&mut socket);
            write_response(
                &mut socket,
                "503 Service Unavailable",
                r#"{"error":"down"}"#,
                &[],
            );
        });

        let result = http_json_request(
            "GET",
            &format!("http://{addr}"),
            "/fail",
            None,
            &[],
            request_options(&any_policy(), None, Some(1_000)),
        );
        assert!(result.is_err());
        let error = result.expect_err("transport failure expected").to_string();
        assert!(error.contains("status 503"));
        assert!(error.contains("down"));
        handle.join().expect("join server thread");
    }

    #[test]
    fn http_json_request_rejects_malformed_or_non_certificate_ca_before_connecting() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let cases: [(&str, &[u8], &str); 3] = [
            (
                "malformed.pem",
                b"not a certificate",
                "invalid CA certificate",
            ),
            (
                "private-key.pem",
                b"-----BEGIN NOT-A-CERTIFICATE-----\nnot a certificate\n-----END NOT-A-CERTIFICATE-----\n",
                "invalid CA certificate",
            ),
            (
                "malformed-certificate.pem",
                b"-----BEGIN CERTIFICATE-----\nZmFrZQ==\n-----END CERTIFICATE-----\n",
                "failed building reqwest HTTP client",
            ),
        ];

        for (file_name, contents, expected_error) in cases {
            let ca_path = tempdir.path().join(file_name);
            fs::write(&ca_path, contents).expect("write invalid CA");
            let error = http_json_request(
                "GET",
                "http://127.0.0.1:1",
                "/unreachable",
                None,
                &[],
                request_options(&any_policy(), Some(&ca_path), Some(1_000)),
            )
            .expect_err("invalid CA must fail before connecting");
            assert!(error.to_string().contains(expected_error), "{error:#}");
        }
    }
}
