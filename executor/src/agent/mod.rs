pub mod anthropic;
pub mod decision;
pub mod prompt;

pub use decision::StrategyDecision;
pub use anthropic::AnthropicClient;

use crate::orders::{OrderManagementSystem, StrategyType};
use anyhow::Result;
use tracing::info;

// ── Agent entry point ─────────────────────────────────────────────────────────
// Called by the main loop when the Julia rule engine flags needs_llm = true.
// Receives the full ZetaState context string + specific questions,
// calls Anthropic, parses the structured decision, routes to OMS.

pub struct StrategyAgent {
    client: AnthropicClient,
}

impl StrategyAgent {
    pub fn new(api_key: String) -> Self {
        StrategyAgent {
            client: AnthropicClient::new(api_key),
        }
    }

    pub async fn consult(
        &self,
        zeta_context: &str,         // output of zeta_context_string() from Julia
        llm_questions: &[String],   // specific ambiguity flags from rule engine
        rule_proposal: &RuleProposal,
    ) -> Result<StrategyDecision> {
        let system = prompt::build_system_prompt();
        let user   = prompt::build_user_prompt(zeta_context, llm_questions, rule_proposal);

        info!(
            strategy = ?rule_proposal.strategy_type,
            contracts = rule_proposal.contracts,
            "Consulting LLM agent"
        );

        let decision = self.client.call_with_tools(&system, &user).await?;

        info!(
            approved  = decision.approved,
            contracts = decision.contracts,
            confidence = decision.confidence,
            "LLM decision received"
        );

        Ok(decision)
    }
}

// Mirrors the Julia StrategyProposal fields we care about
#[derive(Debug, Clone)]
pub struct RuleProposal {
    pub strategy_type:    String,
    pub contracts:        u32,
    pub max_risk_dollars: f64,
    pub est_delta:        f64,
    pub est_vega:         f64,
    pub est_theta_day:    f64,
    pub target_dte:       u32,
    pub entry_urgency:    String,
    pub rationale:        String,
}
