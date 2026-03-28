use super::{ToolCategory, ToolContext, ToolDef, ToolError};
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

/// Handle `activity_heatmap` — returns real data from the ActivityTracker
/// when available, otherwise an empty heatmap.
pub async fn handle_activity_heatmap(
    input: Value,
    ctx: &dyn ToolContext,
) -> Result<Value, ToolError> {
    let days = input
        .get("days")
        .and_then(|v| v.as_u64())
        .unwrap_or(30);

    let character = ctx.character_name();
    let autonomy = ctx.autonomy_manager();

    let stats_opt = autonomy.and_then(|mgr| mgr.activity_stats(character));

    match stats_opt {
        Some((stats, message_count)) => {
            let hours: Vec<Value> = (0..24)
                .map(|h| {
                    let class = match stats.hour_classifications[h] {
                        crate::autonomy::activity::HourClassification::Peak => "peak",
                        crate::autonomy::activity::HourClassification::Trough => "trough",
                        crate::autonomy::activity::HourClassification::Normal => "normal",
                    };
                    json!({
                        "hour": h,
                        "density": stats.hour_histogram[h],
                        "classification": class,
                    })
                })
                .collect();

            Ok(json!({
                "days": days,
                "hours": hours,
                "total_messages": message_count,
                "has_sufficient_data": stats.has_sufficient_heatmap,
                "engagement_score": stats.engagement_score,
                "sessions_per_day": stats.sessions_per_day,
            }))
        }
        None => {
            // No autonomy data available — return empty heatmap.
            Ok(json!({
                "days": days,
                "hours": (0..24).map(|h| json!({ "hour": h, "density": 0.0, "classification": "normal" })).collect::<Vec<_>>(),
                "total_messages": 0,
                "has_sufficient_data": false,
                "engagement_score": 0.0,
                "sessions_per_day": 0.0,
            }))
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestToolContext;

    #[test]
    fn test_activity_tool_defs() {
        let defs = tool_defs();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "activity_heatmap");
        assert_eq!(defs[0].category, ToolCategory::Other);
    }

    #[tokio::test]
    async fn test_activity_heatmap_no_autonomy() {
        // TestToolContext has no autonomy manager — should return empty heatmap.
        let ctx = TestToolContext::new();
        let result = handle_activity_heatmap(json!({}), &ctx).await.unwrap();
        assert_eq!(result["days"], 30);
        assert_eq!(result["total_messages"], 0);
        assert_eq!(result["has_sufficient_data"], false);

        let hours = result["hours"].as_array().unwrap();
        assert_eq!(hours.len(), 24);
    }

    #[tokio::test]
    async fn test_activity_heatmap_custom_days() {
        let ctx = TestToolContext::new();
        let result = handle_activity_heatmap(json!({"days": 7}), &ctx).await.unwrap();
        assert_eq!(result["days"], 7);
    }
}
