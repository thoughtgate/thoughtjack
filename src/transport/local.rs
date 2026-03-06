//! Local-only request validation helpers.
//!
//! Shared DNS rebinding and loopback-peer guards used by both the v0.2
//! HTTP transport and the v0.5 A2A server driver.
//!
//! See TJ-SPEC-002 F-003 and TJ-SPEC-017 F-001.

use std::net::SocketAddr;

use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

/// Validates that the remote peer is loopback.
///
/// # Errors
///
/// Returns a 403 `Response` if the peer address is not loopback.
///
/// Implements: TJ-SPEC-002 F-003
#[allow(clippy::result_large_err)]
pub fn validate_local_peer(addr: SocketAddr) -> Result<(), Response> {
    if addr.ip().is_loopback() {
        Ok(())
    } else {
        Err((StatusCode::FORBIDDEN, "non-local peer rejected").into_response())
    }
}

/// Validates that the request originates from a local address.
///
/// Rejects requests with no `Origin` or `Host` header, or with a header
/// pointing to a non-local hostname. Comparison is case-insensitive.
///
/// # Errors
///
/// Returns a 403 `Response` if the origin header is missing or non-local.
///
/// Implements: TJ-SPEC-002 F-003
#[allow(clippy::result_large_err)]
pub fn validate_local_origin(headers: &HeaderMap) -> Result<(), Response> {
    let origin = headers
        .get("origin")
        .or_else(|| headers.get("host"))
        .and_then(|v| v.to_str().ok());
    let Some(header_value) = origin else {
        return Err((StatusCode::FORBIDDEN, "missing Origin or Host header").into_response());
    };
    let Some(hostname) = extract_hostname_for_origin_check(header_value) else {
        return Err((StatusCode::FORBIDDEN, "dns rebinding rejected").into_response());
    };
    if !matches!(
        hostname.as_str(),
        "localhost" | "127.0.0.1" | "[::1]" | "::1" | "0.0.0.0"
    ) {
        return Err((StatusCode::FORBIDDEN, "dns rebinding rejected").into_response());
    }
    Ok(())
}

/// Extracts a normalized hostname from Origin/Host values.
///
/// Handles both:
/// - Origin values with scheme (e.g. `http://localhost:3000`, `http://[::1]:3000`)
/// - Host values without scheme (e.g. `localhost:3000`, `[::1]:3000`)
///
/// Implements: TJ-SPEC-002 F-003
#[must_use]
pub fn extract_hostname_for_origin_check(header_value: &str) -> Option<String> {
    let authority = if header_value.contains("://") {
        header_value
            .parse::<Uri>()
            .ok()?
            .authority()?
            .as_str()
            .to_string()
    } else {
        header_value.to_string()
    };

    if authority == "::1" {
        return Some("::1".to_string());
    }

    if let Some(stripped) = authority.strip_prefix('[') {
        let end = stripped.find(']')?;
        return Some(format!("[{}]", &stripped[..end]).to_ascii_lowercase());
    }

    Some(
        authority
            .split(':')
            .next()
            .unwrap_or(authority.as_str())
            .to_ascii_lowercase(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_ipv4_accepted() {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        assert!(validate_local_peer(addr).is_ok());
    }

    #[test]
    fn loopback_ipv6_accepted() {
        let addr: SocketAddr = "[::1]:8080".parse().unwrap();
        assert!(validate_local_peer(addr).is_ok());
    }

    #[test]
    fn non_loopback_rejected() {
        let addr: SocketAddr = "192.168.1.1:8080".parse().unwrap();
        assert!(validate_local_peer(addr).is_err());
    }

    #[test]
    fn hostname_from_origin_with_scheme() {
        assert_eq!(
            extract_hostname_for_origin_check("http://localhost:3000"),
            Some("localhost".to_string())
        );
    }

    #[test]
    fn hostname_from_host_without_scheme() {
        assert_eq!(
            extract_hostname_for_origin_check("localhost:3000"),
            Some("localhost".to_string())
        );
    }

    #[test]
    fn hostname_ipv6_origin() {
        assert_eq!(
            extract_hostname_for_origin_check("http://[::1]:3000"),
            Some("[::1]".to_string())
        );
    }

    #[test]
    fn hostname_bare_ipv6() {
        assert_eq!(
            extract_hostname_for_origin_check("::1"),
            Some("::1".to_string())
        );
    }

    #[test]
    fn validate_origin_missing_header() {
        let headers = HeaderMap::new();
        assert!(validate_local_origin(&headers).is_err());
    }

    #[test]
    fn validate_origin_non_local() {
        let mut headers = HeaderMap::new();
        headers.insert("host", "evil.example.com".parse().unwrap());
        assert!(validate_local_origin(&headers).is_err());
    }

    #[test]
    fn validate_origin_localhost() {
        let mut headers = HeaderMap::new();
        headers.insert("host", "localhost:8080".parse().unwrap());
        assert!(validate_local_origin(&headers).is_ok());
    }
}
