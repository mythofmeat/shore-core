# hello — minimal MCP server

A one-tool MCP server used to verify Shore's outbound MCP plumbing
end-to-end. Has no real-world utility; remove from your config once you've
confirmed plumbing works.

## Tool

- `say_hello(name: str) -> str` — returns `"Hello, <name>!"`.

## Requirements

```sh
pip install --user mcp
```

## Run standalone

```sh
python3 dev/mcp-servers/hello/hello.py
```

The server speaks MCP over stdio. To smoke-test without Shore, use the
official `mcp` CLI:

```sh
mcp dev dev/mcp-servers/hello/hello.py
```

## Wire into Shore

Add to `~/.config/shore/config.toml`:

```toml
[mcp.servers.hello]
command = "python3"
args = ["/abs/path/to/dev/mcp-servers/hello/hello.py"]
allowed_tools = ["say_hello"]
enabled = true
```

After restarting `shore-daemon`, ask a character to call
`mcp__hello__say_hello` with `{"name": "world"}`. You should see the daemon
log spawn / handshake / `tools/list` lines and the character receive
`"Hello, world!"`.
