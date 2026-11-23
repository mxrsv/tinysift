//! A host's site: its registrable domain (eTLD+1) on the Public Suffix
//! List, the unit a cookie is kept or removed by.

use std::net::IpAddr;

/// The site of a cookie host or a URL host. A leading dot (a domain
/// cookie) and a trailing dot are dropped and case is folded. An IP
/// address, or a name that is itself a public suffix (`localhost`), is its
/// own site. `None` when nothing is left.
pub(super) fn site_of_host(host: &str) -> Option<String> {
    let host = host
        .trim()
        .trim_start_matches('.')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if bare.parse::<IpAddr>().is_ok() {
        return Some(bare.to_owned());
    }
    Some(psl::domain_str(&host).map_or_else(|| host.clone(), str::to_owned))
}

/// The host of an `http` or `https` URL; `None` for other schemes, which
/// set no cookies a site could be judged by.
pub(super) fn host_of_url(url: &str) -> Option<&str> {
    let (scheme, rest) = url.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if host_port.starts_with('[') {
        host_port
            .split_once(']')
            .map_or(host_port, |(h, _)| &h[1..])
    } else {
        host_port.split(':').next().unwrap_or_default()
    };
    (!host.is_empty()).then_some(host)
}

/// The site of an `http`/`https` URL.
pub(super) fn site_of_url(url: &str) -> Option<String> {
    host_of_url(url).and_then(site_of_host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_hosts_reduce_to_their_registrable_domain() {
        assert_eq!(site_of_host(".example.com").as_deref(), Some("example.com"));
        assert_eq!(
            site_of_host("www.Example.COM").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            site_of_host("a.b.example.co.uk").as_deref(),
            Some("example.co.uk")
        );
        assert_eq!(
            site_of_host("shop.example.com.").as_deref(),
            Some("example.com")
        );
        assert_eq!(site_of_host("192.0.2.7").as_deref(), Some("192.0.2.7"));
        assert_eq!(
            site_of_host("[2001:db8::1]").as_deref(),
            Some("2001:db8::1")
        );
        assert_eq!(site_of_host("localhost").as_deref(), Some("localhost"));
        assert_eq!(site_of_host("."), None);
    }

    #[test]
    fn url_hosts_skip_userinfo_port_and_other_schemes() {
        assert_eq!(
            host_of_url("https://mail.example.com/inbox?x=1"),
            Some("mail.example.com")
        );
        assert_eq!(
            host_of_url("http://user:pw@example.org:8080/"),
            Some("example.org")
        );
        assert_eq!(
            host_of_url("https://[2001:db8::1]:443/a"),
            Some("2001:db8::1")
        );
        assert_eq!(host_of_url("https://example.net#top"), Some("example.net"));
        assert_eq!(host_of_url("file:///Users/someone/a.html"), None);
        assert_eq!(host_of_url("chrome://settings"), None);
        assert_eq!(host_of_url("not a url"), None);
        assert_eq!(
            site_of_url("https://news.example.co.uk/x").as_deref(),
            Some("example.co.uk")
        );
    }
}
