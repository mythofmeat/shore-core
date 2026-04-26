# Tech Debt Tracker

Small, concrete cleanup tasks belong here when they are too small for an active
execution plan but important enough to preserve.

| Item | Area | Why it matters | Suggested validation |
| --- | --- | --- | --- |
| Add markdown link checking | Docs | Prevent stale system-of-record links | `python3 scripts/harness-check.py` plus link checker |
| Add recorded provider fixtures | `shore-llm` | Avoid relying only on hand-written provider fakes | provider fixture tests |
| Expand MCP examples | `dev/mcp` | Make end-to-end agent verification easier | MCP integration test or transcript |
