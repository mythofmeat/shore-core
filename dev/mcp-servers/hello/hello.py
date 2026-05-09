"""Minimal MCP server with one tool, used to smoke-test Shore's outbound
MCP plumbing. See dev/mcp-servers/hello/README.md for usage."""

from mcp.server.fastmcp import FastMCP

mcp = FastMCP("hello")


@mcp.tool()
def say_hello(name: str) -> str:
    """Return a friendly greeting for the given name."""
    return f"Hello, {name}!"


if __name__ == "__main__":
    mcp.run()
