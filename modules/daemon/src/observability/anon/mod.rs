//! Sensitive-value anonymization for LLM contexts.
//!
//! Secrets matched by the compiled pattern set are replaced with stable,
//! deterministic tokens of the form `<<R-xxxxxxxx>>` (the first 8 hex digits
//! of an xxhash64 digest of the secret). The same secret always maps to the
//! same token, keeping provider prefix caches effective. Tokens are restored
//! with [`Anonymizer::deanonymize`] before tool arguments are executed.

use std::collections::HashMap;

use metteur_shared::config::AnonymizeConfig;
use regex::Regex;
use tokio::sync::RwLock;
use twox_hash::XxHash64;

/// Built-in secret patterns redacted before content enters an LLM context.
const BUILTIN_PATTERNS: &[&str] = &[
    // OpenAI-style API keys.
    r"sk-[A-Za-z0-9_-]{10,}",
    // AWS access key ids.
    r"\bAKIA[0-9A-Z]{16}\b",
    // JWTs (`header.payload.signature`).
    r"eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{8,}",
    // PEM private key blocks (may span multiple lines).
    r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----",
    // Credential assignments such as `api_key=xxx` or `password: xxx`.
    r#"(?i)\b(api[_-]?key|secret|token|password)\s*[=:]\s*\S+"#,
];

/// Shape of a replacement token, used to locate tokens for restoration.
const TOKEN_PATTERN: &str = r"<<R-[0-9a-f]{8}>>";

/// Replaces and restores sensitive values in text.
pub struct Anonymizer {
    enabled: bool,
    /// Individual regexes used to locate match spans.
    regexes: Vec<Regex>,
    token_re: Regex,
    map: RwLock<HashMap<String, String>>,
}

impl Anonymizer {
    /// Creates an enabled anonymizer from the built-in patterns plus extras.
    pub fn new(extra_patterns: &[String]) -> Self {
        Self::build(true, extra_patterns)
    }

    /// Creates a disabled anonymizer passing content through unchanged.
    pub fn disabled() -> Self {
        Self::build(false, &[])
    }

    /// Creates an anonymizer from the anonymization configuration.
    pub fn from_config(config: &AnonymizeConfig) -> Self {
        Self::build(config.enabled, &config.extra_patterns)
    }

    fn build(enabled: bool, extra_patterns: &[String]) -> Self {
        let mut sources: Vec<&str> = BUILTIN_PATTERNS.to_vec();
        let mut validated_extra: Vec<String> = Vec::new();
        for pattern in extra_patterns {
            if Regex::new(pattern).is_ok() {
                validated_extra.push(pattern.clone());
            } else {
                tracing::warn!("skipping invalid anonymization pattern '{pattern}'");
            }
        }
        sources.extend(validated_extra.iter().map(|p| p.as_str()));
        let regexes: Vec<Regex> =
            sources.iter().map(|p| Regex::new(p).expect("valid pattern")).collect();
        Self {
            enabled,
            regexes,
            token_re: Regex::new(TOKEN_PATTERN).expect("token pattern is valid"),
            map: RwLock::new(HashMap::new()),
        }
    }

    /// Returns whether anonymization is active.
    pub fn is_enabled(&self) -> bool {
        self.enabled && !self.regexes.is_empty()
    }

    /// Replaces every secret in `text` with a stable opaque token.
    pub async fn anonymize(&self, text: &str) -> String {
        if !self.is_enabled() {
            return text.to_string();
        }
        let mut spans: Vec<(usize, usize)> = Vec::new();
        for re in &self.regexes {
            for m in re.find_iter(text) {
                spans.push((m.start(), m.end()));
            }
        }
        if spans.is_empty() {
            return text.to_string();
        }
        spans.sort_unstable();
        // Merge overlapping spans, keeping the earliest / widest match.
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(spans.len());
        for (start, end) in spans {
            match merged.last_mut() {
                Some(last) if start < last.1 => {
                    if end > last.1 {
                        last.1 = end;
                    }
                }
                _ => merged.push((start, end)),
            }
        }
        let mut map = self.map.write().await;
        let mut out = String::with_capacity(text.len());
        let mut pos = 0;
        for (start, end) in merged {
            let secret = &text[start..end];
            let token = token_for(secret);
            out.push_str(&text[pos..start]);
            out.push_str(&token);
            map.entry(token).or_insert_with(|| secret.to_string());
            pos = end;
        }
        out.push_str(&text[pos..]);
        out
    }

    /// Restores previously anonymized tokens in `text` to their secrets.
    pub async fn deanonymize(&self, text: &str) -> String {
        if !self.is_enabled() {
            return text.to_string();
        }
        let map = self.map.read().await;
        if map.is_empty() || !self.token_re.is_match(text) {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut pos = 0;
        for m in self.token_re.find_iter(text) {
            if let Some(secret) = map.get(m.as_str()) {
                out.push_str(&text[pos..m.start()]);
                out.push_str(secret);
                pos = m.end();
            }
        }
        out.push_str(&text[pos..]);
        out
    }
}

/// Computes the deterministic replacement token for `secret`.
///
/// The token embeds the first 8 hex digits of the xxhash64 digest so that the
/// same secret always yields the same token across runs.
fn token_for(secret: &str) -> String {
    use std::hash::Hasher;
    let mut hasher = XxHash64::with_seed(0);
    hasher.write(secret.as_bytes());
    let digest = hasher.finish();
    format!("<<R-{:08x}>>", (digest >> 32) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const JWT: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N0XgL0n3I9PlFUP0THsR8U";

    #[tokio::test]
    async fn builtin_patterns_are_redacted() {
        let a = Anonymizer::new(&[]);
        let text = format!(
            "openai sk-abcdefgh12345678901234 aws AKIAIOSFODNN7EXAMPLE jwt {JWT} \
             api_key=super-secret-value\n-----BEGIN RSA PRIVATE KEY-----\nabc\ndef\n-----END RSA PRIVATE KEY-----"
        );
        let out = a.anonymize(&text).await;
        assert!(!out.contains("sk-abcdefgh"));
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(!out.contains("eyJhbGciOiJIUzI1NiIsInR5"));
        assert!(!out.contains("super-secret-value"));
        assert!(!out.contains("PRIVATE KEY---\nabc"));
        assert!(out.contains("<<R-"));
        assert!(a.is_enabled());
    }

    #[tokio::test]
    async fn same_input_yields_same_token() {
        let a = Anonymizer::new(&[]);
        let b = Anonymizer::new(&[]);
        let first = a.anonymize("api_key=hunter2").await;
        let second = b.anonymize("api_key=hunter2").await;
        assert_eq!(first, second);
        assert_ne!(first, "api_key=hunter2");
    }

    #[tokio::test]
    async fn round_trip_restores_original_text() {
        let a = Anonymizer::new(&[]);
        let original = "connect with sk-abcdefgh12345678901234 and api_key=zzz then done";
        let anonymized = a.anonymize(original).await;
        assert_ne!(anonymized, original);
        let restored = a.deanonymize(&anonymized).await;
        assert_eq!(restored, original);
    }

    #[tokio::test]
    async fn disabled_anonymizer_is_a_passthrough() {
        let config = AnonymizeConfig::default();
        let a = Anonymizer::from_config(&config);
        assert!(!a.is_enabled());
        let original = "sk-abcdefgh12345678901234";
        assert_eq!(a.anonymize(original).await, original);
        assert_eq!(a.deanonymize(original).await, original);
    }

    #[tokio::test]
    async fn extra_patterns_from_config_are_applied() {
        let config = AnonymizeConfig {
            enabled: true,
            extra_patterns: vec![r"PROJ-[0-9]{6}".to_string()],
        };
        let a = Anonymizer::from_config(&config);
        let original = "ticket PROJ-123456 filed";
        let out = a.anonymize(original).await;
        assert!(!out.contains("PROJ-123456"));
        assert_eq!(a.deanonymize(&out).await, original);
    }
}