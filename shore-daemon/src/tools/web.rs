use super::{ToolCategory, ToolDef, ToolError};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

pub fn tool_defs() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "web_search",
            description: "Search the web using Tavily or a configurable search provider.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query."
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum number of results to return.",
                        "default": 5
                    }
                },
                "required": ["query"]
            }),
            category: ToolCategory::Web,
        },
        ToolDef {
            name: "fetch_url",
            description: "Fetch and read the content of a web page.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to fetch."
                    }
                },
                "required": ["url"]
            }),
            category: ToolCategory::Web,
        },
        ToolDef {
            name: "research_web",
            description: "Perform multi-step web research on a topic, combining multiple searches and page reads.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "topic": {
                        "type": "string",
                        "description": "The research topic or question."
                    },
                    "depth": {
                        "type": "string",
                        "description": "Research depth: 'shallow' (1-2 searches) or 'deep' (3+ searches with cross-referencing).",
                        "default": "shallow"
                    }
                },
                "required": ["topic"]
            }),
            category: ToolCategory::Web,
        },
    ]
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Handle `web_search` — search the web.
/// Stub: full implementation requires a configured search API (e.g. Tavily).
pub async fn handle_web_search(input: Value) -> Result<Value, ToolError> {
    let query = input
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing 'query' field".to_string()))?;

    let max_results = input
        .get("max_results")
        .and_then(|v| v.as_u64())
        .unwrap_or(5);

    // Stub — full implementation calls Tavily/SearXNG API via reqwest.
    Err(ToolError::NotImplemented(format!(
        "web_search (query={}, max_results={}): requires search API configuration",
        query, max_results
    )))
}

/// Handle `fetch_url` — fetch a webpage.
/// Stub: full implementation uses reqwest to GET the URL and extract text.
pub async fn handle_fetch_url(input: Value) -> Result<Value, ToolError> {
    let url = input
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing 'url' field".to_string()))?;

    // Stub — full implementation fetches via reqwest and extracts readable text.
    Err(ToolError::NotImplemented(format!(
        "fetch_url (url={}): requires HTTP client",
        url
    )))
}

/// Handle `research_web` — multi-step web research.
/// Stub: full implementation orchestrates multiple search + fetch cycles.
pub async fn handle_research_web(input: Value) -> Result<Value, ToolError> {
    let topic = input
        .get("topic")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing 'topic' field".to_string()))?;

    let depth = input
        .get("depth")
        .and_then(|v| v.as_str())
        .unwrap_or("shallow");

    // Stub — full implementation chains web_search + fetch_url calls.
    Err(ToolError::NotImplemented(format!(
        "research_web (topic={}, depth={}): requires search API configuration",
        topic, depth
    )))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_web_tool_defs() {
        let defs = tool_defs();
        assert_eq!(defs.len(), 3);

        let names: Vec<&str> = defs.iter().map(|d| d.name).collect();
        assert!(names.contains(&"web_search"));
        assert!(names.contains(&"fetch_url"));
        assert!(names.contains(&"research_web"));

        // All web tools should have Web category.
        for def in &defs {
            assert_eq!(def.category, ToolCategory::Web);
        }
    }

    #[tokio::test]
    async fn test_web_search_missing_query() {
        let result = handle_web_search(json!({})).await;
        assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
    }

    #[tokio::test]
    async fn test_web_search_stub() {
        let result = handle_web_search(json!({"query": "rust programming"})).await;
        assert!(matches!(result, Err(ToolError::NotImplemented(_))));
    }

    #[tokio::test]
    async fn test_fetch_url_missing_url() {
        let result = handle_fetch_url(json!({})).await;
        assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
    }

    #[tokio::test]
    async fn test_fetch_url_stub() {
        let result = handle_fetch_url(json!({"url": "https://example.com"})).await;
        assert!(matches!(result, Err(ToolError::NotImplemented(_))));
    }

    #[tokio::test]
    async fn test_research_web_missing_topic() {
        let result = handle_research_web(json!({})).await;
        assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
    }

    #[tokio::test]
    async fn test_research_web_stub() {
        let result = handle_research_web(json!({"topic": "AI safety"})).await;
        assert!(matches!(result, Err(ToolError::NotImplemented(_))));
    }
}
