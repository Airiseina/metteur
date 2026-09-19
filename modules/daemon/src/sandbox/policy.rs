//! Command policy matching for the sandbox.

use metteur_shared::config::SandboxConfig;

/// Classification of a command against the sandbox policy lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyVerdict {
    /// Explicitly whitelisted; may run without approval.
    Allowed,
    /// Blacklisted; always requires approval with a deny suggestion.
    Denied,
    /// Not present in either list.
    Unknown,
}

/// Returns the policy verdict for a command line.
pub fn classify(command: &str, config: &SandboxConfig) -> PolicyVerdict {
    if matches_any(command, &config.blacklist) {
        return PolicyVerdict::Denied;
    }
    if matches_any(command, &config.whitelist) {
        return PolicyVerdict::Allowed;
    }
    PolicyVerdict::Unknown
}

fn matches_any(command: &str, patterns: &[String]) -> bool {
    let normalized = normalize(command);
    patterns.iter().any(|pattern| wildcard_match(&normalize(pattern), &normalized))
}

/// Lowercases and collapses whitespace for stable comparisons and grant keys.
pub fn normalize(command: &str) -> String {
    command.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Case-insensitive glob match supporting `*` wildcards.
pub fn wildcard_match(pattern: &str, value: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let value: Vec<char> = value.chars().collect();
    let (mut pi, mut vi) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while vi < value.len() {
        if pi < pattern.len() && pattern[pi] == value[vi] {
            pi += 1;
            vi += 1;
        } else if pi < pattern.len() && pattern[pi] == '*' {
            star = Some(pi);
            mark = vi;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            vi = mark;
        } else {
            return false;
        }
    }
    while pi < pattern.len() && pattern[pi] == '*' {
        pi += 1;
    }
    pi == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(whitelist: &[&str], blacklist: &[&str]) -> SandboxConfig {
        SandboxConfig {
            enabled: true,
            whitelist: whitelist.iter().map(|s| s.to_string()).collect(),
            blacklist: blacklist.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn whitelist_prefix_glob_matches() {
        let cfg = config(&["cargo *", "git status"], &[]);
        assert_eq!(classify("cargo build --release", &cfg), PolicyVerdict::Allowed);
        assert_eq!(classify("cargo", &cfg), PolicyVerdict::Unknown);
        assert_eq!(classify("GIT   STATUS", &cfg), PolicyVerdict::Allowed);
        assert_eq!(classify("git push", &cfg), PolicyVerdict::Unknown);
    }

    #[test]
    fn blacklist_takes_precedence() {
        let cfg = config(&["rm *"], &["rm -rf /*"]);
        assert_eq!(classify("rm -rf /", &cfg), PolicyVerdict::Denied);
    }

    #[test]
    fn unknown_command_is_unclassified() {
        let cfg = config(&["node *"], &[]);
        assert_eq!(classify("python train.py", &cfg), PolicyVerdict::Unknown);
    }

    #[test]
    fn wildcard_star_only_pattern() {
        assert!(wildcard_match("*", "anything at all"));
        assert!(!wildcard_match("ab*cd", "abcdx"));
        assert!(wildcard_match("ab*cd", "abxxcd"));
    }
}
