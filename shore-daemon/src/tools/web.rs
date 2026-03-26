use super::{ToolCategory, ToolDef, ToolError};
use serde_json::{json, Value};
use std::time::Duration;

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

/// Maximum content length returned to the model (chars).
const MAX_CONTENT_CHARS: usize = 50_000;

/// Handle `fetch_url` — fetch a webpage and extract readable text.
pub async fn handle_fetch_url(input: Value) -> Result<Value, ToolError> {
    let url = input
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing 'url' field".to_string()))?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("shore/2.0")
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| ToolError::Http(format!("failed to build HTTP client: {e}")))?;

    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| ToolError::Http(format!("request failed: {e}")))?;

    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        return Err(ToolError::Http(format!(
            "HTTP {status} for {url}"
        )));
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    let body = resp
        .text()
        .await
        .map_err(|e| ToolError::Http(format!("failed to read body: {e}")))?;

    let is_html = content_type.contains("html");
    let content = if is_html {
        strip_html(&body)
    } else {
        body
    };

    let truncated = content.len() > MAX_CONTENT_CHARS;
    let content = if truncated {
        let boundary = content.floor_char_boundary(MAX_CONTENT_CHARS);
        content[..boundary].to_string()
    } else {
        content
    };

    Ok(json!({
        "url": url,
        "content_type": content_type,
        "content": content,
        "truncated": truncated,
    }))
}

/// Strip HTML tags and extract readable text content.
///
/// Removes `<script>`, `<style>`, and `<head>` blocks entirely, strips remaining
/// tags, decodes common HTML entities, and collapses whitespace.
fn strip_html(html: &str) -> String {
    // Phase 1: Remove script, style, and head blocks
    let mut cleaned = String::with_capacity(html.len());
    let lower = html.to_lowercase();
    let lower_bytes = lower.as_bytes();
    let mut i = 0;

    while i < html.len() {
        if lower_bytes[i] == b'<' {
            // Check for blocks we want to skip entirely
            let remaining = &lower[i..];
            if let Some(tag) = ["script", "style", "head"]
                .iter()
                .find(|t| remaining.starts_with(&format!("<{}", t)))
            {
                // Find the closing tag
                let close = format!("</{tag}");
                if let Some(end_pos) = lower[i..].find(&close) {
                    // Skip past the closing tag's '>'
                    let after_close = i + end_pos + close.len();
                    if let Some(gt) = lower[after_close..].find('>') {
                        i = after_close + gt + 1;
                        continue;
                    }
                }
                // No closing tag found — skip to end
                break;
            }

            // Regular tag — skip it but add a space (block elements create breaks)
            if let Some(gt) = html[i..].find('>') {
                cleaned.push(' ');
                i += gt + 1;
                continue;
            }
        }

        // Consume one character
        if let Some(ch) = html[i..].chars().next() {
            cleaned.push(ch);
            i += ch.len_utf8();
        } else {
            break;
        }
    }

    // Phase 2: Decode common HTML entities
    let decoded = cleaned
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&#x27;", "'")
        .replace("&#x2F;", "/");

    // Phase 3: Collapse whitespace
    let mut result = String::with_capacity(decoded.len());
    let mut prev_whitespace = false;
    for ch in decoded.chars() {
        if ch.is_whitespace() {
            if !prev_whitespace {
                result.push(' ');
            }
            prev_whitespace = true;
        } else {
            result.push(ch);
            prev_whitespace = false;
        }
    }

    result.trim().to_string()
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

    #[test]
    fn test_strip_html_basic() {
        let html = "<html><body><h1>Hello</h1><p>World</p></body></html>";
        let text = strip_html(html);
        assert!(text.contains("Hello"));
        assert!(text.contains("World"));
        assert!(!text.contains("<h1>"));
    }

    #[test]
    fn test_strip_html_removes_script_and_style() {
        let html = r#"<html><head><title>T</title></head><body>
            <script>var x = 1;</script>
            <style>.foo { color: red; }</style>
            <p>Visible text</p>
        </body></html>"#;
        let text = strip_html(html);
        assert!(text.contains("Visible text"));
        assert!(!text.contains("var x"));
        assert!(!text.contains("color: red"));
        assert!(!text.contains("<title>"));
    }

    #[test]
    fn test_strip_html_decodes_entities() {
        let html = "<p>A &amp; B &lt; C &gt; D &quot;E&quot;</p>";
        let text = strip_html(html);
        assert!(text.contains("A & B < C > D \"E\""));
    }

    #[test]
    fn test_strip_html_collapses_whitespace() {
        let html = "<p>  lots   of    spaces  </p>";
        let text = strip_html(html);
        // Should not have runs of multiple spaces
        assert!(!text.contains("  "));
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
