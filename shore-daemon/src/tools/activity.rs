use super::{ToolCategory, ToolDef, ToolError};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

pub fn tool_defs() -> Vec<ToolDef> {
    vec![ToolDef {
        name: "activity_heatmap",
        description: "Show the user's message activity patterns as a heatmap by hour of day.",
        parameters: json!({
            "type": "object",
            "properties": {
                "days": {
                    "type": "integer",
                    "description": "Number of days of history to include.",
                    "default": 30
                }
            }
        }),
        category: ToolCategory::Other,
    }]
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// Handle `activity_heatmap` — stub returning placeholder data.
/// Full implementation in Phase 4 (Autonomy subsystem).
pub async fn handle_activity_heatmap(input: Value) -> Result<Value, ToolError> {
    let days = input
        .get("days")
        .and_then(|v| v.as_u64())
        .unwrap_or(30);

    // Stub: return empty heatmap structure.
    Ok(json!({
        "days": days,
        "hours": (0..24).map(|h| json!({ "hour": h, "count": 0 })).collect::<Vec<_>>(),
        "total_messages": 0,
        "note": "Activity heatmap stub — full implementation in Phase 4.",
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_activity_tool_defs() {
        let defs = tool_defs();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "activity_heatmap");
        assert_eq!(defs[0].category, ToolCategory::Other);
    }

    #[tokio::test]
    async fn test_activity_heatmap_stub() {
        let result = handle_activity_heatmap(json!({})).await.unwrap();
        assert_eq!(result["days"], 30);
        assert_eq!(result["total_messages"], 0);

        let hours = result["hours"].as_array().unwrap();
        assert_eq!(hours.len(), 24);
    }

    #[tokio::test]
    async fn test_activity_heatmap_custom_days() {
        let result = handle_activity_heatmap(json!({"days": 7})).await.unwrap();
        assert_eq!(result["days"], 7);
    }
}
