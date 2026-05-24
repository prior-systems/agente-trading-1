use serde::{Deserialize, Serialize};

// ── Structured decision output from the LLM agent ────────────────────────────
// Produced by tool_use — the model is forced to call submit_decision()
// with this exact schema. No free-form text that needs parsing.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyDecision {
    // Core decision
    pub approved: bool,

    // Strategy — may differ from rule engine proposal if LLM modifies it
    pub strategy_type: String,
    pub contracts: u32,

    // Greeks targets (LLM can tighten or widen)
    pub target_delta: f64,
    pub target_vega:  f64,

    // Execution parameters
    pub target_dte: u32,            // days to expiry
    pub entry_urgency: String,      // "immediate" | "patient" | "conditional"

    // Reasoning (required — used for audit log and human review)
    pub reasoning: String,

    // Confidence 0..1
    pub confidence: f64,

    // Override or refinements (null if no changes to rule engine proposal)
    pub macro_concerns: Option<String>,  // e.g. "FOMC in 3 days — reduce size"
    pub sizing_adjustment: Option<f64>,  // multiplier on contracts (e.g. 0.5 = half size)
    pub conditional_trigger: Option<String>, // e.g. "wait for VIX < 20"
}

// The tool definition sent to Anthropic to force structured output
pub fn decision_tool_definition() -> serde_json::Value {
    serde_json::json!({
        "name": "submit_decision",
        "description": "Submit the final trading strategy decision. You MUST call this tool — do not respond with text only.",
        "input_schema": {
            "type": "object",
            "required": [
                "approved", "strategy_type", "contracts",
                "target_delta", "target_vega",
                "target_dte", "entry_urgency",
                "reasoning", "confidence"
            ],
            "properties": {
                "approved": {
                    "type": "boolean",
                    "description": "Whether to proceed with the trade"
                },
                "strategy_type": {
                    "type": "string",
                    "enum": [
                        "IronCondor", "Strangle", "IronButterfly",
                        "LongStraddle", "LongStrangle", "Backspread",
                        "RiskReversal", "FuturesCalendar",
                        "FuturesLong", "FuturesShort",
                        "DeltaHedge", "DoNothing"
                    ],
                    "description": "Strategy type — may keep or change the rule engine proposal"
                },
                "contracts": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Number of contracts. 0 = do not trade"
                },
                "target_delta": {
                    "type": "number",
                    "description": "Target net portfolio delta after trade"
                },
                "target_vega": {
                    "type": "number",
                    "description": "Expected net vega of the new position (negative = short vol)"
                },
                "target_dte": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Target days to expiration"
                },
                "entry_urgency": {
                    "type": "string",
                    "enum": ["immediate", "patient", "conditional"],
                    "description": "How urgently to enter"
                },
                "reasoning": {
                    "type": "string",
                    "description": "Mandatory explanation of the decision, addressing the specific ambiguity questions raised"
                },
                "confidence": {
                    "type": "number",
                    "minimum": 0.0,
                    "maximum": 1.0,
                    "description": "Confidence in this decision given available information"
                },
                "macro_concerns": {
                    "type": ["string", "null"],
                    "description": "Any macro/calendar concerns that affected the decision"
                },
                "sizing_adjustment": {
                    "type": ["number", "null"],
                    "minimum": 0.0,
                    "maximum": 2.0,
                    "description": "Multiplier on rule-engine contract count (1.0 = unchanged, 0.5 = half size)"
                },
                "conditional_trigger": {
                    "type": ["string", "null"],
                    "description": "If entry_urgency is conditional, describe the trigger condition"
                }
            }
        }
    })
}
