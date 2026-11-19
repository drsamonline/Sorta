//! LLM tiers.
//!
//! Sorta's core features never need a language model. A model only adds
//! natural-language rules, multi-step workflows and free-form chat. So the tier
//! ladder is a ladder of *extras*, and each rung degrades into the one below:
//!
//! | Tier | What | Cost |
//! |---|---|---|
//! | 0 | rules + ONNX embeddings + regex NER | none, always works |
//! | 1 | a local server already running (Ollama, LM Studio, llama.cpp, LocalAI) | none |
//! | 2 | bundled SmolLM2-135M (~85 MB, opt-in download) | disk |
//! | 3 | BYOK: OpenAI, Anthropic, Groq, OpenRouter, Gemini, any compatible API | money, privacy |
//!
//! Phase 1 ships the probe (so `sorta status` tells the truth about what is
//! available) and the fallback logic. Generation itself lands with the HTTP
//! client in phase 4; `complete()` returns `None` until then, which the caller
//! treats exactly like "no LLM".

use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::{Config, LlmMode};

/// How long a probe waits for a local server.
const PROBE_TIMEOUT: Duration = Duration::from_millis(150);

/// Default local endpoints, in the order they are probed.
pub const DEFAULT_ENDPOINTS: [&str; 4] = [
    "127.0.0.1:11434", // Ollama
    "127.0.0.1:1234",  // LM Studio
    "127.0.0.1:8080",  // llama.cpp server
    "127.0.0.1:5001",  // LocalAI
];

/// Which rung is in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Rules only.
    Zero,
    /// A local server is answering.
    One,
    /// Bundled small model.
    Two,
    /// Bring your own key.
    Three,
}

impl Tier {
    pub fn label(self) -> &'static str {
        match self {
            Self::Zero => "Tier 0 (rules only)",
            Self::One => "Tier 1 (local server)",
            Self::Two => "Tier 2 (bundled model)",
            Self::Three => "Tier 3 (BYOK)",
        }
    }

    /// Whether natural-language rules can be parsed at this tier.
    pub fn supports_nl_rules(self) -> bool {
        self >= Self::Two
    }

    /// Whether multi-step workflows can run at this tier.
    pub fn supports_workflows(self) -> bool {
        self >= Self::One
    }
}

/// Chooses and reports the active tier. Never blocks a sort.
#[derive(Debug, Clone)]
pub struct LlmRouter {
    mode: LlmMode,
    active: Tier,
    probed: bool,
    endpoints: Vec<String>,
    provider: String,
    send_content: bool,
}

impl LlmRouter {
    pub fn new(cfg: &Config) -> Self {
        let endpoints = if cfg.llm_endpoint.trim().is_empty() {
            DEFAULT_ENDPOINTS.iter().map(|e| e.to_string()).collect()
        } else {
            vec![cfg.llm_endpoint.trim().to_string()]
        };

        // The starting tier follows the policy; `probe` may lower it.
        let active = match cfg.llm_mode {
            LlmMode::Off => Tier::Zero,
            LlmMode::Byok => Tier::Three,
            LlmMode::Bundled => Tier::Two,
            LlmMode::Auto | LlmMode::AutoWithFallback => Tier::Zero,
        };

        Self {
            mode: cfg.llm_mode,
            active,
            probed: false,
            endpoints,
            provider: cfg.llm_provider.clone(),
            send_content: cfg.llm_send_content,
        }
    }

    /// Probes local endpoints and settles on a tier.
    ///
    /// Probing is a TCP connect with a 150 ms timeout, not an HTTP request: it
    /// answers "is something listening?" without spending a token or writing a
    /// log line on the other machine.
    pub fn probe(&mut self) -> Tier {
        self.probed = true;
        if self.mode == LlmMode::Off {
            self.active = Tier::Zero;
            return self.active;
        }

        let reachable = self
            .endpoints
            .iter()
            .any(|endpoint| endpoint_reachable(endpoint));

        self.active = match self.mode {
            LlmMode::Off => Tier::Zero,
            LlmMode::Auto | LlmMode::AutoWithFallback => {
                if reachable {
                    Tier::One
                } else {
                    Tier::Zero
                }
            }
            LlmMode::Bundled => Tier::Two,
            LlmMode::Byok => Tier::Three,
        };
        self.active
    }

    /// The tier currently in use.
    pub fn active(&self) -> Tier {
        self.active
    }

    /// Drops one rung. Called once per failure, never in a loop.
    pub fn degrade(&mut self) {
        self.active = match self.active {
            Tier::Three => Tier::Two,
            Tier::Two => Tier::One,
            Tier::One => Tier::Zero,
            Tier::Zero => Tier::Zero,
        };
    }

    /// Whether content may leave the machine.
    pub fn may_send_content(&self) -> bool {
        self.send_content && self.active == Tier::Three
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn endpoints(&self) -> &[String] {
        &self.endpoints
    }

    /// Generation. Returns `None` until the HTTP client lands, which callers
    /// treat as "no model available" and fall through to Tier 0 behaviour.
    pub fn complete(&self, _prompt: &str) -> Option<String> {
        None
    }

    /// One line for `sorta status`.
    pub fn describe(&self) -> String {
        let suffix = if self.probed { "" } else { " (not probed)" };
        format!(
            "{} · mode {} · {}{}",
            self.active.label(),
            self.mode.as_str(),
            if self.may_send_content() {
                "content sharing ON"
            } else {
                "offline"
            },
            suffix
        )
    }
}

fn endpoint_reachable(endpoint: &str) -> bool {
    match endpoint.parse::<SocketAddr>() {
        Ok(addr) => TcpStream::connect_timeout(&addr, PROBE_TIMEOUT).is_ok(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_mode_never_leaves_tier_zero() {
        let mut cfg = Config::default();
        cfg.llm_mode = LlmMode::Off;
        let mut router = LlmRouter::new(&cfg);
        assert_eq!(router.probe(), Tier::Zero);
    }

    #[test]
    fn byok_is_tier_three_and_offline_until_opted_in() {
        let mut cfg = Config::default();
        cfg.llm_mode = LlmMode::Byok;
        cfg.llm_provider = "openai".to_string();
        let router = LlmRouter::new(&cfg);
        assert_eq!(router.active(), Tier::Three);
        assert!(
            !router.may_send_content(),
            "content sharing must default off"
        );
        assert_eq!(router.provider(), "openai");
    }

    #[test]
    fn degrading_walks_down_the_ladder() {
        let mut cfg = Config::default();
        cfg.llm_mode = LlmMode::Byok;
        let mut router = LlmRouter::new(&cfg);
        router.degrade();
        assert_eq!(router.active(), Tier::Two);
        router.degrade();
        assert_eq!(router.active(), Tier::One);
        router.degrade();
        assert_eq!(router.active(), Tier::Zero);
        router.degrade();
        assert_eq!(router.active(), Tier::Zero);
    }

    #[test]
    fn tier_capabilities() {
        assert!(!Tier::Zero.supports_nl_rules());
        assert!(!Tier::Zero.supports_workflows());
        assert!(Tier::Two.supports_nl_rules());
        assert!(Tier::Three.supports_workflows());
    }

    #[test]
    fn generation_is_unavailable_in_phase_one() {
        let router = LlmRouter::new(&Config::default());
        assert!(router.complete("hello").is_none());
    }

    #[test]
    fn custom_endpoint_overrides_defaults() {
        let mut cfg = Config::default();
        cfg.llm_endpoint = "127.0.0.1:9999".to_string();
        let router = LlmRouter::new(&cfg);
        assert_eq!(router.endpoints(), &["127.0.0.1:9999".to_string()]);
    }
}
