//! Basic tools: check_time, roll_dice.
//!
//! Migrated from the legacy `engine/tools.rs` ToolRegistry.

use rand::Rng;
use serde_json::{json, Value};

use super::{ToolCategory, ToolDef, ToolError};

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

pub fn tool_defs() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "check_time",
            description: "Returns the current date and time in ISO 8601 format.",
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            category: ToolCategory::Other,
        },
        ToolDef {
            name: "roll_dice",
            description: "Roll dice using standard dice notation (e.g., '2d6+3', '1d20', 'd8').",
            parameters: json!({
                "type": "object",
                "properties": {
                    "notation": {
                        "type": "string",
                        "description": "Dice notation: NdS[+/-M] where N=number of dice, S=sides, M=modifier. Examples: '2d6', '1d20+5', '4d6-1'"
                    }
                },
                "required": ["notation"]
            }),
            category: ToolCategory::Other,
        },
    ]
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub async fn handle_check_time(_input: Value) -> Result<Value, ToolError> {
    let now = chrono::Local::now();
    Ok(json!(now.to_rfc3339()))
}

pub async fn handle_roll_dice(input: Value) -> Result<Value, ToolError> {
    let notation_str = input
        .get("notation")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing 'notation' parameter".into()))?;

    let parsed = parse_dice_notation(notation_str)
        .map_err(|e| ToolError::InvalidArgs(format!("invalid dice notation: {e}")))?;

    let (rolls, total) = execute_dice_roll(&parsed);
    Ok(json!({
        "notation": notation_str,
        "rolls": rolls,
        "total": total,
    }))
}

// ---------------------------------------------------------------------------
// Dice notation parsing (migrated from engine/tools.rs)
// ---------------------------------------------------------------------------

/// Parsed dice notation (e.g., `2d6+3` → count=2, sides=6, modifier=3).
#[derive(Debug, Clone, PartialEq)]
pub struct DiceNotation {
    pub count: u32,
    pub sides: u32,
    pub modifier: i32,
}

/// Parse dice notation like `2d6+3`, `1d20`, `4d6-1`, `d8`.
pub fn parse_dice_notation(notation: &str) -> Result<DiceNotation, String> {
    let s = notation.trim().to_lowercase();

    let d_pos = s
        .find('d')
        .ok_or_else(|| format!("Missing 'd' in notation: {notation}"))?;

    let count_str = &s[..d_pos];
    let count = if count_str.is_empty() {
        1
    } else {
        count_str
            .parse::<u32>()
            .map_err(|_| format!("Invalid dice count: {count_str}"))?
    };
    if count == 0 {
        return Err("Dice count must be at least 1".into());
    }

    let after_d = &s[d_pos + 1..];
    if after_d.is_empty() {
        return Err("Missing sides after 'd'".into());
    }

    let modifier_pos = after_d
        .char_indices()
        .position(|(i, c)| i > 0 && (c == '+' || c == '-'));

    let (sides_str, modifier) = if let Some(pos) = modifier_pos {
        let byte_pos = after_d
            .char_indices()
            .nth(pos)
            .map(|(i, _)| i)
            .unwrap();
        let sides = &after_d[..byte_pos];
        let mod_str = &after_d[byte_pos..];
        let modifier = mod_str
            .parse::<i32>()
            .map_err(|_| format!("Invalid modifier: {mod_str}"))?;
        (sides, modifier)
    } else {
        (after_d, 0)
    };

    let sides = sides_str
        .parse::<u32>()
        .map_err(|_| format!("Invalid sides: {sides_str}"))?;
    if sides == 0 {
        return Err("Dice sides must be at least 1".into());
    }

    Ok(DiceNotation {
        count,
        sides,
        modifier,
    })
}

/// Roll dice according to parsed notation. Returns (individual rolls, total).
pub fn execute_dice_roll(notation: &DiceNotation) -> (Vec<u32>, i32) {
    let mut rng = rand::thread_rng();
    let rolls: Vec<u32> = (0..notation.count)
        .map(|_| rng.gen_range(1..=notation.sides))
        .collect();
    let sum: i32 = rolls.iter().map(|&r| r as i32).sum::<i32>() + notation.modifier;
    (rolls, sum)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_standard_notation() {
        let r = parse_dice_notation("2d6").unwrap();
        assert_eq!(r, DiceNotation { count: 2, sides: 6, modifier: 0 });
    }

    #[test]
    fn parse_with_positive_modifier() {
        let r = parse_dice_notation("1d20+5").unwrap();
        assert_eq!(r, DiceNotation { count: 1, sides: 20, modifier: 5 });
    }

    #[test]
    fn parse_with_negative_modifier() {
        let r = parse_dice_notation("4d6-1").unwrap();
        assert_eq!(r, DiceNotation { count: 4, sides: 6, modifier: -1 });
    }

    #[test]
    fn parse_implicit_count() {
        let r = parse_dice_notation("d8").unwrap();
        assert_eq!(r, DiceNotation { count: 1, sides: 8, modifier: 0 });
    }

    #[test]
    fn parse_rejects_missing_d() {
        assert!(parse_dice_notation("26").is_err());
    }

    #[test]
    fn parse_rejects_zero_count() {
        assert!(parse_dice_notation("0d6").is_err());
    }

    #[test]
    fn parse_rejects_zero_sides() {
        assert!(parse_dice_notation("2d0").is_err());
    }

    #[test]
    fn dice_roll_within_range() {
        let notation = DiceNotation { count: 2, sides: 6, modifier: 3 };
        for _ in 0..100 {
            let (rolls, total) = execute_dice_roll(&notation);
            assert_eq!(rolls.len(), 2);
            for &r in &rolls {
                assert!((1..=6).contains(&r));
            }
            assert!((5..=15).contains(&total));
        }
    }

    #[tokio::test]
    async fn handle_check_time_returns_datetime() {
        let result = handle_check_time(json!({})).await.unwrap();
        let s = result.as_str().unwrap();
        assert!(s.contains('T'));
    }

    #[tokio::test]
    async fn handle_roll_dice_valid() {
        let result = handle_roll_dice(json!({"notation": "2d6"})).await.unwrap();
        assert_eq!(result["notation"], "2d6");
        assert!(result["rolls"].is_array());
        assert_eq!(result["rolls"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn handle_roll_dice_missing_notation() {
        let result = handle_roll_dice(json!({})).await;
        assert!(result.is_err());
    }
}
