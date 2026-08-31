//! ACL configuration and rule matching.

use serde::{Deserialize, Serialize};

/// A single ACL rule granting or denying access to gRPC method paths.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AclRule {
    /// The subject (client identity) this rule applies to.
    pub subject: String,
    /// Method paths to allow (supports `*` wildcards).
    #[serde(default)]
    pub allow: Vec<String>,
    /// Method paths to deny (supports `*` wildcards).
    #[serde(default)]
    pub deny: Vec<String>,
}

/// The ACL configuration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AclConfig {
    /// Ordered list of rules; the first matching rule wins.
    #[serde(default)]
    pub rules: Vec<AclRule>,
}

impl AclConfig {
    /// Returns whether the given subject is allowed to call `method_path`.
    ///
    /// Rules are evaluated in order; the first rule whose subject matches and
    /// whose allow/deny list matches the path decides the outcome. With no
    /// rules configured, access is allowed by default (the daemon is
    /// local-first); once rules exist, an unmatched subject is denied.
    pub fn is_allowed(&self, subject: &str, method_path: &str) -> bool {
        if self.rules.is_empty() {
            return true;
        }
        for rule in &self.rules {
            if !wildcard_match(&rule.subject, subject) {
                continue;
            }
            if rule.deny.iter().any(|p| wildcard_match(p, method_path)) {
                return false;
            }
            if rule.allow.iter().any(|p| wildcard_match(p, method_path)) {
                return true;
            }
        }
        false
    }
}

/// Matches `pattern` against `value`, where `*` matches any sequence of chars.
fn wildcard_match(pattern: &str, value: &str) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let mut p = 0;
    let mut v = 0;
    let mut star: Option<usize> = None;
    let mut mark = 0;

    while v < value.len() {
        if p < pattern.len() && (pattern[p] == b'*' || pattern[p] == value[v]) {
            if pattern[p] == b'*' {
                star = Some(p);
                mark = v;
            }
            p += 1;
            v += 1;
        } else if let Some(sp) = star {
            p = sp + 1;
            mark += 1;
            v = mark;
        } else {
            return false;
        }
    }

    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_matches() {
        assert!(wildcard_match("/metteur.Workspace/*", "/metteur.Workspace/OpenWorkspace"));
        assert!(!wildcard_match("/metteur.Workspace/*", "/metteur.Config/GetConfig"));
        assert!(wildcard_match("*", "/anything/at/all"));
        assert!(wildcard_match("/a/b", "/a/b"));
        assert!(!wildcard_match("/a/b", "/a/c"));
    }

    #[test]
    fn deny_overrides_allow() {
        let cfg = AclConfig {
            rules: vec![AclRule {
                subject: "client1".to_string(),
                allow: vec!["/metteur.Workspace/*".to_string()],
                deny: vec!["/metteur.Workspace/CloseWorkspace".to_string()],
            }],
        };
        assert!(cfg.is_allowed("client1", "/metteur.Workspace/OpenWorkspace"));
        assert!(!cfg.is_allowed("client1", "/metteur.Workspace/CloseWorkspace"));
        assert!(!cfg.is_allowed("other", "/metteur.Workspace/OpenWorkspace"));
    }

    #[test]
    fn empty_rules_allow_by_default() {
        let cfg = AclConfig::default();
        assert!(cfg.is_allowed("local", "/metteur.Workspace/OpenWorkspace"));
        assert!(cfg.is_allowed("anyone", "/anything/at/all"));
    }

    #[test]
    fn subject_wildcard_grants_any_identity() {
        let cfg = AclConfig {
            rules: vec![AclRule {
                subject: "*".to_string(),
                allow: vec!["/metteur.Daemon/ListWorkspaces".to_string()],
                deny: vec![],
            }],
        };
        // The `*` subject matches every identity, including the local one.
        assert!(cfg.is_allowed("client1", "/metteur.Daemon/ListWorkspaces"));
        assert!(cfg.is_allowed("local", "/metteur.Daemon/ListWorkspaces"));
        // But only the listed method is granted.
        assert!(!cfg.is_allowed("client1", "/metteur.Daemon/OpenWorkspace"));
    }
}
