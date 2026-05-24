use super::RuleProposal;

pub fn build_system_prompt() -> String {
    r#"You are a senior volatility trader reviewing a proposed options trade.

Your role is NOT to generate strategy ideas from scratch — the quantitative rule engine
has already done that using the Zeta Field state (vol surface, Greeks, regime, order flow).
Your role is to resolve specific ambiguities the rule engine flagged, using your knowledge
of macro context, market structure, and trading experience.

CONSTRAINTS — these are non-negotiable:
- Hard risk limits (max delta, max vega, max loss per trade) are enforced by the system.
  You cannot override them. Your job is only to approve, modify sizing, or block.
- If you lack sufficient information to resolve an ambiguity, set approved=false and explain why.
- Never invent market data. Reason only from what is provided in the Zeta Field context.
- You MUST call the submit_decision tool — text-only responses are not processed.

DECISION FRAMEWORK:
1. Read the Zeta Field state carefully — it encodes vol regime, smile geometry, order flow.
2. Address each ambiguity question specifically in your reasoning.
3. If a macro event (FOMC, earnings, CPI) is imminent and not priced into the field,
   reduce sizing (sizing_adjustment < 1.0) or set entry_urgency="conditional".
4. If the field signals conflict and you cannot resolve them, set approved=false.
5. The strategy_type and contracts in your response override the rule engine proposal.
   If you agree with the proposal, echo it unchanged."#
    .to_string()
}

pub fn build_user_prompt(
    zeta_context: &str,
    questions: &[String],
    proposal: &RuleProposal,
) -> String {
    let questions_block = if questions.is_empty() {
        "None — reviewing as a general sanity check.".to_string()
    } else {
        questions
            .iter()
            .enumerate()
            .map(|(i, q)| format!("{}. {}", i + 1, q))
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        r#"{zeta_context}

═══════════════════════════════════════════════════════
RULE ENGINE PROPOSAL
═══════════════════════════════════════════════════════
Strategy:      {strategy}
Contracts:     {contracts}
Max risk:      ${max_risk:.0}
Est delta:     {delta:.3}
Est vega:      {vega:.2}
Est theta/day: {theta:.2}
Target DTE:    {dte} days
Urgency:       {urgency}
Rationale:     {rationale}

═══════════════════════════════════════════════════════
AMBIGUITY FLAGS — address each one in your reasoning
═══════════════════════════════════════════════════════
{questions}

Review the above and call submit_decision with your verdict."#,
        zeta_context = zeta_context,
        strategy     = proposal.strategy_type,
        contracts    = proposal.contracts,
        max_risk     = proposal.max_risk_dollars,
        delta        = proposal.est_delta,
        vega         = proposal.est_vega,
        theta        = proposal.est_theta_day,
        dte          = proposal.target_dte,
        urgency      = proposal.entry_urgency,
        rationale    = proposal.rationale,
        questions    = questions_block,
    )
}
