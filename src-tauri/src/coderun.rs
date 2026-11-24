//! Allowlisted command runner for the coding mode's `run_command` tool. The model may run
//! build/test/lint style commands so it can see real errors and fix them, but only commands whose
//! leading tokens match an allowlist derived from the project (e.g. `cargo test`, `npm run build`).
//! The command string is validated to contain no shell metacharacters, so it cannot chain, redirect
//! or substitute to escape the allowlist. Output is captured, capped to the tail (where build errors
//! live) and handed back to the model. Never logs command output — only the command and exit code.

use crate::errors::{AppError, AppResult};
use std::path::Path;
use std::time::Duration;

/// Each command's own wall-clock limit (build/test can be slow; a hung server is killed here).
pub const PER_CMD: Duration = Duration::from_secs(90);
/// Longest command string accepted.
const MAX_CMD_LEN: usize = 400;
/// Characters of combined output returned to the model (tail kept).
const OUTPUT_CHARS: usize = 6000;

/// Result of a finished (or timed-out) command, trimmed for the model.
pub struct CmdOutcome {
    /// process exit code; `None` when it timed out or was killed by a signal
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// combined stdout+stderr, capped
    pub output: String,
}

/// Characters allowed in a command string. Everything else — shell metacharacters (`| & ; < > ( )`),
/// quotes, `$`, `%`, `*`, backtick, `~`, newline — is refused, so nothing is left for a shell to
/// interpret and the command cannot chain or redirect.
fn is_ok_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || " .,/\\-_=:@".contains(c)
}

/// Reject a command that is empty, too long, or contains a disallowed (shell-dangerous) character.
pub fn sanitize(command: &str) -> AppResult<()> {
    let c = command.trim();
    if c.is_empty() {
        return Err(AppError::new("code_cmd", "ไม่มีคำสั่ง"));
    }
    if c.len() > MAX_CMD_LEN {
        return Err(AppError::new("code_cmd", "คำสั่งยาวเกินกำหนด"));
    }
    if !c.chars().all(is_ok_char) {
        return Err(AppError::new(
            "code_cmd",
            "คำสั่งมีอักขระที่ไม่อนุญาต (ห้ามใช้ | & ; < > ( ) ' \" $ % * ` ฯลฯ — รันได้ทีละคำสั่งเท่านั้น)",
        ));
    }
    Ok(())
}

/// Default allowed command prefixes for a project, by the marker files at its root. Kept to
/// build/test/check/lint/run-script — not install (network + install scripts) — so auto-running them
/// is safe. The user can add more in Settings.
pub fn default_allow(root: &Path) -> Vec<String> {
    let has = |f: &str| root.join(f).exists();
    let mut v: Vec<String> = Vec::new();
    let add = |v: &mut Vec<String>, items: &[&str]| v.extend(items.iter().map(|s| s.to_string()));
    if has("Cargo.toml") {
        add(&mut v, &["cargo check", "cargo build", "cargo test", "cargo clippy", "cargo fmt", "cargo run"]);
    }
    if has("package.json") {
        add(&mut v, &["npm run", "npm test", "npx tsc", "node", "pnpm run", "pnpm test", "yarn build", "yarn test", "yarn run", "bun run", "bun test"]);
    }
    if has("pyproject.toml") || has("requirements.txt") || has("setup.py") || has("setup.cfg") {
        add(&mut v, &["python", "python3", "pytest", "ruff", "mypy", "flake8"]);
    }
    if has("go.mod") {
        add(&mut v, &["go build", "go test", "go vet", "go run"]);
    }
    v
}

/// A command is allowed when its leading whitespace-separated tokens match one of the allowlist
/// prefixes exactly (so `cargo test --lib` matches the prefix `cargo test`).
pub fn is_allowed(command: &str, allow: &[String]) -> bool {
    let toks: Vec<&str> = command.split_whitespace().collect();
    allow.iter().any(|a| {
        let want: Vec<&str> = a.split_whitespace().collect();
        !want.is_empty() && want.len() <= toks.len() && want.iter().zip(&toks).all(|(w, t)| w == t)
    })
}

/// Trim long output, keeping the tail — build/test errors are usually at the end.
fn cap(s: &str) -> String {
    let n = s.chars().count();
    if n <= OUTPUT_CHARS {
        return s.to_string();
    }
    let tail: String = s.chars().skip(n - OUTPUT_CHARS).collect();
    format!("…(ตัดส่วนต้น)\n{tail}")
}

/// Run `command` in `root` without a shell interpreting metacharacters (it has already been
/// sanitized). stdin is closed and common prompts/pagers/colors are disabled so nothing blocks on
/// input. On timeout the child is killed (`kill_on_drop`) and a timed-out outcome is returned.
pub async fn run(root: &Path, command: &str, timeout: Duration) -> AppResult<CmdOutcome> {
    use std::process::Stdio;
    use tokio::process::Command;

    // `cmd /C` / `sh -c` so Windows .cmd shims (npm/npx/tsc) and PATH lookup work; the string carries
    // no metacharacters, so the shell only locates and runs the one program with its args.
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(command);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(command);
        c
    };
    cmd.current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("CI", "1")
        .env("NO_COLOR", "1")
        .env("FORCE_COLOR", "0")
        .env("CARGO_TERM_COLOR", "never")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat");

    let child = cmd.spawn().map_err(|_| AppError::new("code_cmd", "เรียกคำสั่งไม่ได้"))?;
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => {
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            let err = String::from_utf8_lossy(&out.stderr);
            if !err.trim().is_empty() {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&err);
            }
            if text.trim().is_empty() {
                text = "(ไม่มีข้อความออกจากคำสั่ง)".into();
            }
            Ok(CmdOutcome { exit_code: out.status.code(), timed_out: false, output: cap(&text) })
        }
        Ok(Err(_)) => Err(AppError::new("code_cmd", "รันคำสั่งไม่สำเร็จ")),
        // dropping the future here kills the child (kill_on_drop)
        Err(_) => Ok(CmdOutcome {
            exit_code: None,
            timed_out: true,
            output: format!("คำสั่งใช้เวลาเกิน {} วินาที จึงถูกหยุด", timeout.as_secs()),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn sanitize_rejects_shell_metacharacters() {
        assert!(sanitize("cargo test").is_ok());
        assert!(sanitize("npm run build").is_ok());
        assert!(sanitize("node scripts/x.js --flag=1").is_ok());
        for bad in ["rm -rf / && echo", "cat a | sh", "echo $(whoami)", "x > out.txt", "a; b", "echo `id`", "ls *"] {
            assert!(sanitize(bad).is_err(), "should reject: {bad}");
        }
        assert!(sanitize("   ").is_err());
    }

    #[test]
    fn allowlist_matches_by_leading_tokens() {
        let allow = vec!["cargo test".to_string(), "npm run".to_string(), "node".to_string()];
        assert!(is_allowed("cargo test --lib", &allow));
        assert!(is_allowed("npm run build", &allow));
        assert!(is_allowed("node app.js", &allow));
        assert!(!is_allowed("cargo publish", &allow));
        assert!(!is_allowed("npm install", &allow), "install is not in the list");
        assert!(!is_allowed("cargo", &allow), "a bare prefix word is not the full prefix");
        assert!(!is_allowed("rm -rf", &allow));
    }

    #[test]
    fn default_allow_follows_project_markers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        let allow = default_allow(dir.path());
        assert!(allow.iter().any(|a| a == "cargo test"));
        assert!(!allow.iter().any(|a| a.starts_with("npm")), "no package.json → no npm");

        let empty = default_allow(&PathBuf::from(dir.path().join("does-not-exist")));
        assert!(empty.is_empty());
    }

    #[tokio::test]
    async fn runs_a_command_and_captures_output() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(dir.path(), "echo hello-from-test", Duration::from_secs(20)).await.unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert!(!out.timed_out);
        assert!(out.output.contains("hello-from-test"), "got: {}", out.output);
    }
}
