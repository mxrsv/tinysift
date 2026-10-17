//! Ownership rules for uninstall leftovers (spec rule O1).

/// Longest bundle identifier accepted in a path.
const MAX_BUNDLE_ID_LEN: usize = 255;

/// True when `id` is safe to join into a `~/Library` path: reverse-DNS with at
/// least two non-empty dot-separated components of ASCII letters, digits or `-`.
/// This excludes `/`, `..`, `.`, whitespace and control characters by construction.
pub(crate) fn valid_bundle_id(id: &str) -> bool {
    id.len() <= MAX_BUNDLE_ID_LEN
        && id.split('.').count() >= 2
        && id.split('.').all(|part| {
            !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_or_malformed_ids() {
        let too_long = format!("com.{}", "a".repeat(MAX_BUNDLE_ID_LEN));
        for id in [
            ".",
            "..",
            "../..",
            "a/b",
            "",
            "com",
            "com..x",
            "com.",
            ".com",
            "com. x",
            "com.ex\u{7}ample",
            "com.ex\nample",
            "com.exämple",
            "com.my_app",
            too_long.as_str(),
        ] {
            assert!(!valid_bundle_id(id), "{id:?} must be rejected");
        }
    }

    #[test]
    fn accepts_reverse_dns_ids() {
        for id in ["com.example.App", "com.example.my-app", "org.x2.y"] {
            assert!(valid_bundle_id(id), "{id:?} must be accepted");
        }
    }
}
