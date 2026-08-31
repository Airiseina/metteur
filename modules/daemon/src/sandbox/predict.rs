//! Heuristic impact prediction for shell commands.

/// Risk classification of a predicted command impact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    /// Read-only or trivially safe.
    Low,
    /// Creates or modifies files within the workspace.
    Medium,
    /// Deletes data or may damage the system.
    High,
    /// The command could not be understood.
    Unknown,
}

impl Risk {
    /// Returns the stable lowercase name used in reports and audit entries.
    pub fn as_str(&self) -> &'static str {
        match self {
            Risk::Low => "low",
            Risk::Medium => "medium",
            Risk::High => "high",
            Risk::Unknown => "unknown",
        }
    }
}

/// The predicted impact of a command line.
#[derive(Debug, Clone)]
pub struct ImpactReport {
    /// The program being invoked (first token).
    pub program: String,
    /// Paths the command is predicted to touch.
    pub affected_paths: Vec<String>,
    /// Overall risk level.
    pub risk: Risk,
}

/// Predicts the files affected by a command and its overall risk.
///
/// The analysis is heuristic: it understands common file-manipulating
/// commands and shell redirections, and reports [`Risk::Unknown`] for
/// anything it cannot parse so users can review the raw command instead.
pub fn predict(command: &str) -> ImpactReport {
    let tokens = tokenize(command);
    let Some(program) = tokens.first().cloned() else {
        return ImpactReport {
            program: String::new(),
            affected_paths: Vec::new(),
            risk: Risk::Unknown,
        };
    };

    // Windows `cmd /C <inner>` style invocations are re-analyzed from the
    // inner command so builtin verbs are recognized.
    if let Some(inner) = inner_command(&tokens) {
        return predict(&inner);
    }

    let base = program_base(&program);
    if base == "cmd" || base == "powershell" {
        return ImpactReport {
            program,
            affected_paths: Vec::new(),
            risk: Risk::Unknown,
        };
    }

    let args: Vec<String> = tokens[1..].iter().filter(|a| !is_flag(a)).cloned().collect();
    let redirects = redirection_targets(command);

    match base.as_str() {
        "rm" | "del" | "erase" | "rd" | "rmdir" | "format" | "diskpart" | "reg" => ImpactReport {
            program,
            affected_paths: args,
            risk: Risk::High,
        },
        "copy" | "cp" | "move" | "mv" | "mkdir" | "md" | "touch" => ImpactReport {
            program,
            affected_paths: args,
            risk: Risk::Medium,
        },
        "echo" | "printf" if !redirects.is_empty() => ImpactReport {
            program,
            affected_paths: redirects,
            risk: Risk::Medium,
        },
        "echo" | "printf" | "type" | "cat" | "ls" | "dir" => ImpactReport {
            program,
            affected_paths: Vec::new(),
            risk: Risk::Low,
        },
        _ => ImpactReport {
            program,
            affected_paths: Vec::new(),
            risk: Risk::Unknown,
        },
    }
}

/// Extracts the inner command from `cmd /C ...` style invocations.
fn inner_command(tokens: &[String]) -> Option<String> {
    let idx = tokens.iter().position(|t| {
        let lower = t.to_lowercase();
        lower == "/c" || lower == "/k"
    })?;
    if idx + 1 >= tokens.len() && tokens.len() == idx + 1 {
        return None;
    }
    Some(tokens[idx + 1..].join(" "))
}

/// Returns the lowercase basename of a program path.
fn program_base(program: &str) -> String {
    let base = program.rsplit(['/', '\\']).next().unwrap_or(program);
    base.strip_suffix(".exe").unwrap_or(base).to_lowercase()
}

fn is_flag(arg: &str) -> bool {
    arg.starts_with('-') || arg.starts_with('/')
}

/// Extracts redirect targets (`>`, `>>`) from a raw command line.
fn redirection_targets(command: &str) -> Vec<String> {
    let parts: Vec<&str> = command.split_whitespace().collect();
    let mut out = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        let target = if *part == ">" || *part == ">>" {
            parts.get(i + 1).map(|t| t.trim_matches('"').to_string())
        } else if part.len() > 1 && part.starts_with('>') {
            Some(part.trim_start_matches('>').trim_matches('"').to_string())
        } else {
            None
        };
        if let Some(t) = target {
            out.push(t);
        }
    }
    out
}

/// Splits a command line into tokens, honoring single and double quotes.
fn tokenize(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in command.chars() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                } else {
                    current.push(ch);
                }
            }
            None => match ch {
                '\'' | '"' => quote = Some(ch),
                c if c.is_whitespace() => {
                    if !current.is_empty() {
                        tokens.push(std::mem::take(&mut current));
                    }
                }
                c => current.push(c),
            },
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_commands_are_high_risk() {
        let report = predict("rm -rf build/");
        assert_eq!(report.risk, Risk::High);
        assert!(report.affected_paths.contains(&"build/".to_string()));

        let report = predict("del /q tmp.txt");
        assert_eq!(report.risk, Risk::High);
    }

    #[test]
    fn copy_commands_are_medium_risk() {
        let report = predict("cp src.txt dst.txt");
        assert_eq!(report.risk, Risk::Medium);
        assert_eq!(report.affected_paths.len(), 2);
    }

    #[test]
    fn echo_redirection_is_medium_and_read_only_low() {
        let report = predict("echo hello > out.txt");
        assert_eq!(report.risk, Risk::Medium);
        assert_eq!(report.affected_paths, vec!["out.txt".to_string()]);

        assert_eq!(predict("cat notes.md").risk, Risk::Low);
    }

    #[test]
    fn unknown_programs_report_unknown_risk() {
        let report = predict("python train.py --epochs 3");
        assert_eq!(report.risk, Risk::Unknown);
        assert_eq!(report.program, "python");
    }

    #[test]
    fn cmd_inner_command_is_reanalyzed() {
        let report = predict("cmd /C del foo.txt");
        assert_eq!(report.risk, Risk::High);
        assert_eq!(report.program, "del");
    }

    #[test]
    fn quoted_paths_tokenize_correctly() {
        let report = predict(r#"copy "my file.txt" backup"#);
        assert_eq!(report.affected_paths[0], "my file.txt");
    }
}
