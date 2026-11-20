//! Post-sort hooks.
//!
//! A hook is a shell command the user writes, run after a file has been moved and
//! verified. It exists so Sorta can feed the rest of someone's toolchain without
//! Sorta growing integrations: notify a chat, touch an index, bump a counter.
//!
//! Three rules make this safe to have in a file sorter:
//!
//! * **Opt-in.** No template, no process. The default is empty.
//! * **Never blocking.** The hook is spawned, not awaited: a hook that hangs must
//!   not stall a sort, so its output is discarded and its exit code is not read.
//! * **Quoted.** Every substituted value is shell-quoted, so a filename containing
//!   `; rm -rf ~` is data, not a second command.

use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::config::HooksConfig;

/// Values a hook template can interpolate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookContext {
    /// Where the file ended up.
    pub path: String,
    /// Where it came from.
    pub src: String,
    /// Rule that decided, e.g. `keyword:invoice`.
    pub rule: String,
    pub confidence: f32,
}

/// Renders and spawns the configured hook.
#[derive(Debug, Clone)]
pub struct HookRunner {
    template: String,
}

impl HookRunner {
    pub fn from_config(config: &HooksConfig) -> Self {
        Self {
            template: config.on_sort.trim().to_string(),
        }
    }

    /// True when a hook is configured.
    pub fn is_enabled(&self) -> bool {
        !self.template.is_empty()
    }

    /// The command line, with every value quoted.
    pub fn render(&self, context: &HookContext) -> String {
        self.template
            .replace("{path}", &quote(&context.path))
            .replace("{src}", &quote(&context.src))
            .replace("{rule}", &quote(&context.rule))
            .replace("{confidence}", &format!("{:.2}", context.confidence))
    }

    /// Spawns the hook. `true` means the process started, not that it succeeded.
    pub fn run(&self, context: &HookContext) -> bool {
        if !self.is_enabled() {
            return false;
        }
        spawn_shell(&self.render(context)).is_ok()
    }
}

#[cfg(unix)]
fn spawn_shell(command: &str) -> std::io::Result<std::process::Child> {
    Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

#[cfg(windows)]
fn spawn_shell(command: &str) -> std::io::Result<std::process::Child> {
    Command::new("cmd")
        .arg("/C")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

/// Quotes a value for the platform shell.
#[cfg(unix)]
fn quote(value: &str) -> String {
    // Single quotes protect everything except a single quote itself, which is
    // closed, escaped and reopened.
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(windows)]
fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> HookContext {
        HookContext {
            path: "/data/Sorted/2026/invoice.pdf".to_string(),
            src: "/inbox/invoice.pdf".to_string(),
            rule: "keyword:invoice".to_string(),
            confidence: 0.94,
        }
    }

    fn runner(template: &str) -> HookRunner {
        HookRunner::from_config(&HooksConfig {
            on_sort: template.to_string(),
        })
    }

    #[test]
    fn no_template_means_no_hook() {
        let runner = runner("   ");
        assert!(!runner.is_enabled());
        assert!(!runner.run(&context()));
    }

    #[test]
    fn renders_every_token() {
        let rendered = runner("notify {path} {src} {rule} {confidence}").render(&context());
        assert!(rendered.contains("invoice.pdf"));
        assert!(rendered.contains("keyword:invoice"));
        assert!(rendered.contains("0.94"));
        assert!(!rendered.contains('{'));
    }

    #[cfg(unix)]
    #[test]
    fn quotes_values_so_shell_metacharacters_are_data() {
        let hostile = HookContext {
            path: "/tmp/a; touch /tmp/pwned.pdf".to_string(),
            src: "/tmp/b".to_string(),
            rule: "format:text".to_string(),
            confidence: 0.5,
        };
        let rendered = runner("echo {path}").render(&hostile);
        assert_eq!(rendered, "echo '/tmp/a; touch /tmp/pwned.pdf'");

        // A quote inside the value cannot close the quoting.
        let quoted = quote("it's fine");
        assert_eq!(quoted, r"'it'\''s fine'");
    }

    #[cfg(unix)]
    #[test]
    fn a_hook_actually_runs() {
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("hook ran");
        let template = format!("printf '%s' {{path}} > '{}'", marker.display());
        let runner = runner(&template);

        assert!(runner.run(&context()), "hook should spawn");

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if marker.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let written =
            std::fs::read_to_string(&marker).expect("hook should have written the marker");
        assert_eq!(written, "/data/Sorted/2026/invoice.pdf");
    }
}
