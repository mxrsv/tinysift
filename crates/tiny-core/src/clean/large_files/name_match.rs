//! Matching an installer's file name to an app's name, and comparing the
//! versions both carry. Only the name decides the match (IN1): version,
//! architecture and "installer" tokens are cut off, case and separators are
//! ignored, and nothing is guessed beyond that.

/// Tokens that describe a build, not the app. The first token of a name is
/// never cut, so an app called "Mac Mouse Fix" keeps its name.
const NOISE: &[&str] = &[
    "arm64",
    "aarch64",
    "x86_64",
    "x64",
    "x86",
    "amd64",
    "universal",
    "universal2",
    "apple",
    "silicon",
    "intel",
    "macos",
    "osx",
    "darwin",
    "mac",
    "installer",
    "setup",
    "install",
    "dmg",
    "pkg",
    "beta",
];

/// What a file or app name says: the name with every separator removed, and
/// the version it carries, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedName {
    pub key: String,
    pub version: Option<Vec<u64>>,
}

/// `"TinySift-1.4.2_arm64"` becomes key `tinysift`, version `[1, 4, 2]`.
/// `stem` is the name without its extension.
pub(super) fn parse_name(stem: &str) -> ParsedName {
    let lowered = stem.to_lowercase();
    let tokens = lowered
        .split(|c: char| !(c.is_alphanumeric() || c == '.'))
        .map(|token| token.trim_matches('.'))
        .filter(|token| !token.is_empty());
    let mut key = String::new();
    let mut version = None;
    let mut in_name = true;
    for (index, token) in tokens.enumerate() {
        let (word, embedded) = split_embedded_version(token);
        if in_name && index == 0 {
            key.push_str(word);
            version = embedded.and_then(parse_version);
            in_name = embedded.is_none();
            continue;
        }
        if in_name && (is_version_token(token) || NOISE.contains(&token)) {
            in_name = false;
        }
        if in_name {
            key.push_str(word);
            if let Some(embedded) = embedded {
                version = version.or_else(|| parse_version(embedded));
                in_name = false;
            }
        } else if version.is_none() && is_version_token(token) {
            version = parse_version(token);
        }
    }
    ParsedName { key, version }
}

/// `"firefox128.0"` is `("firefox", Some("128.0"))`: letters followed by a
/// dotted number. A plain trailing digit ("things3") stays in the name.
fn split_embedded_version(token: &str) -> (&str, Option<&str>) {
    for (index, c) in token.char_indices().skip(1) {
        if !c.is_ascii_digit() {
            continue;
        }
        let (word, rest) = token.split_at(index);
        let letter_before = word.chars().next_back().is_some_and(char::is_alphabetic);
        if letter_before && rest.contains('.') && is_dotted_number(rest) {
            return (word, Some(rest));
        }
    }
    (token, None)
}

fn is_dotted_number(text: &str) -> bool {
    text.starts_with(|c: char| c.is_ascii_digit())
        && text.chars().all(|c| c.is_ascii_digit() || c == '.')
        && !text.contains("..")
}

/// `1.4.2`, `v1.14.1` or a bare `11`.
fn is_version_token(token: &str) -> bool {
    is_dotted_number(token.strip_prefix('v').unwrap_or(token))
}

/// The leading dotted number of `text` (`"v1.4.2"`, `"128.0.1 beta"`), or
/// `None` when it does not start with one.
pub(super) fn parse_version(text: &str) -> Option<Vec<u64>> {
    let text = text.trim();
    let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
    let end = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let numeric = text[..end].trim_end_matches('.');
    if numeric.is_empty() {
        return None;
    }
    numeric
        .split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect()
}

/// `installed` is strictly newer than `installer`; missing trailing parts
/// count as zero (`1.4` equals `1.4.0`).
pub(super) fn is_newer(installed: &[u64], installer: &[u64]) -> bool {
    let len = installed.len().max(installer.len());
    for index in 0..len {
        let (a, b) = (
            installed.get(index).copied().unwrap_or(0),
            installer.get(index).copied().unwrap_or(0),
        );
        if a != b {
            return a > b;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(stem: &str) -> String {
        parse_name(stem).key
    }

    #[test]
    fn version_and_architecture_tokens_are_cut() {
        assert_eq!(key("TinySift-1.4.2"), "tinysift");
        assert_eq!(key("TinySift_arm64"), "tinysift");
        assert_eq!(key("tinysift v1.14.1"), "tinysift");
        assert_eq!(key("Cap-darwin-arm64"), "cap");
        assert_eq!(key("Visual Studio Code"), "visualstudiocode");
        assert_eq!(key("visual-studio-code-darwin-arm64"), "visualstudiocode");
        assert_eq!(key("Firefox128.0"), "firefox");
        assert_eq!(key("Slack-4.38.121-macOS-universal"), "slack");
    }

    #[test]
    fn the_first_token_is_never_cut_and_digits_inside_a_name_stay() {
        assert_eq!(key("Mac Mouse Fix"), "macmousefix");
        assert_eq!(key("Things3"), "things3");
        assert_eq!(key("1Password-8.10.30"), "1password");
        assert_eq!(key("Node.js-v18.0.0"), "node.js");
    }

    #[test]
    fn the_version_is_the_first_one_found() {
        assert_eq!(parse_name("TinySift-1.4.2").version, Some(vec![1, 4, 2]));
        assert_eq!(
            parse_name("tool_v1.14.1_arm64").version,
            Some(vec![1, 14, 1])
        );
        assert_eq!(parse_name("Firefox128.0").version, Some(vec![128, 0]));
        assert_eq!(parse_name("TinySift").version, None);
        assert_eq!(parse_name("Windows 11 installer").version, Some(vec![11]));
    }

    #[test]
    fn versions_parse_their_numeric_prefix_only() {
        assert_eq!(parse_version("1.4.2"), Some(vec![1, 4, 2]));
        assert_eq!(parse_version("v2.0"), Some(vec![2, 0]));
        assert_eq!(parse_version("128.0.1 beta"), Some(vec![128, 0, 1]));
        assert_eq!(parse_version("beta"), None);
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("1..2"), None);
        assert_eq!(parse_version("99999999999999999999.1"), None);
    }

    #[test]
    fn newer_means_strictly_newer() {
        assert!(is_newer(&[1, 5], &[1, 4, 2]));
        assert!(!is_newer(&[1, 4, 2], &[1, 4, 2]));
        assert!(!is_newer(&[1, 4], &[1, 4, 0]));
        assert!(!is_newer(&[1, 3, 9], &[1, 4]));
        assert!(is_newer(&[2], &[1, 99]));
    }
}
