//! Reject requests that did not come from the dashboard itself.
//!
//! Windlass has no login; it relies on the LAN and Tailscale. Checking the
//! `Host` header stops DNS rebinding, and checking `Origin` and
//! `Sec-Fetch-Site` on state-changing requests stops other websites from
//! submitting forms to the dashboard.

/// The host name of a `Host` header value, without its port.
fn host_name(host: &str) -> &str {
    if let Some(bracketed) = host.strip_prefix('[') {
        return bracketed.split_once(']').map_or(host, |(name, _)| name);
    }
    match host.rsplit_once(':') {
        Some((name, port))
            if !name.contains(':')
                && !port.is_empty()
                && port.bytes().all(|b| b.is_ascii_digit()) =>
        {
            name
        }
        _ => host,
    }
}

/// Whether the `Host` header names one of the configured host names or addresses.
pub(super) fn host_allowed(host: Option<&str>, allowed: &[String]) -> bool {
    host.is_some_and(|host| {
        let name = host_name(host);
        allowed
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(name))
    })
}

/// Whether a state-changing request came from a page served by this host.
///
/// Browsers send `Origin` with form submissions. A request without it is not
/// from another website's page, so it is allowed.
pub(super) fn same_origin(host: &str, origin: Option<&str>, fetch_site: Option<&str>) -> bool {
    if fetch_site.is_some_and(|site| !matches!(site, "same-origin" | "none")) {
        return false;
    }
    origin.is_none_or(|origin| {
        origin
            .strip_prefix("http://")
            .or_else(|| origin.strip_prefix("https://"))
            .is_some_and(|authority| authority.eq_ignore_ascii_case(host))
    })
}

#[cfg(test)]
mod tests {
    use super::{host_allowed, same_origin};

    fn allowed() -> Vec<String> {
        vec![
            String::from("192.168.1.20"),
            String::from("::1"),
            String::from("windlass.example.ts.net"),
        ]
    }

    #[test]
    fn host_names_match_without_ports_or_case() {
        for host in [
            "192.168.1.20:8740",
            "192.168.1.20",
            "[::1]:8740",
            "[::1]",
            "Windlass.Example.ts.net:8740",
        ] {
            assert!(host_allowed(Some(host), &allowed()), "{host}");
        }
    }

    #[test]
    fn unknown_or_missing_hosts_are_rejected() {
        for host in [
            "attacker.example:8740",
            "192.168.1.21",
            "[::2]:80",
            "192.168.1.20.attacker.example",
            "[::1",
        ] {
            assert!(!host_allowed(Some(host), &allowed()), "{host}");
        }
        assert!(!host_allowed(None, &allowed()));
    }

    #[test]
    fn cross_site_submissions_are_rejected() {
        let host = "192.168.1.20:8740";
        assert!(same_origin(host, None, None));
        assert!(same_origin(host, Some("http://192.168.1.20:8740"), None));
        assert!(same_origin(
            host,
            Some("https://192.168.1.20:8740"),
            Some("same-origin")
        ));
        assert!(same_origin(host, None, Some("none")));
        assert!(!same_origin(host, Some("http://attacker.example"), None));
        assert!(!same_origin(host, Some("null"), None));
        assert!(!same_origin(host, Some("http://192.168.1.20:8741"), None));
        assert!(!same_origin(host, None, Some("cross-site")));
        assert!(!same_origin(host, None, Some("same-site")));
    }
}
