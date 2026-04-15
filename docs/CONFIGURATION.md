# Configuration

Where every Shore setting lives, what it does, and when to change it. For the exhaustive option list see [`examples/config.toml`](../examples/config.toml).

## Orientation

Shore reads all configuration from `$XDG_CONFIG_HOME/shore/` (defaults to `~/.config/shore/`). A minimal install needs one file (`config.toml`) and one character directory (`characters/<Name>/character.md`).

### Directory layout

```
~/.config/shore/
├── config.toml                  # main configuration — required
├── user.md                      # who you are (global fallback) — optional
├── prompts/
│   └── system.md                # system prompt template (global fallback) — optional
└── characters/
    └── <CharacterName>/
        ├── character.md         # required (presence enables discovery)
        ├── user.md              # character-specific override — optional
        └── prompts/
            └── system.md        # character-specific system prompt — optional
```

Characters are discovered by scanning `characters/` for subdirectories containing `character.md`. No config entry is needed to register a character.

### Splitting configuration across files

Two mechanisms let you split config out of the main `config.toml`:

- `include = ["extra.toml", "another.toml"]` at the top of `config.toml` — explicit, order-preserving includes.
- `conf.d/*.toml` — any `.toml` file dropped in `~/.config/shore/conf.d/` is auto-loaded.

Later files override earlier ones. `conf.d/` is loaded in filename order.

### Precedence

For the settings that accept multiple sources, the order (highest wins):

1. CLI flag — `shore-daemon --addr ...`, `shore --character ...`, `shore-daemon --config <path>`
2. Environment variable — `SHORE_ADDR`, `SHORE_CHARACTER`
3. Config file — `[daemon].addr`, etc.

If you pass `--config <path>` the file must already exist. Shore no longer silently creates a default config at an arbitrary operator-supplied path.

Remote-access safety is enforced against the *final* resolved address, so a CLI or env override binding to a non-loopback address still requires `[daemon].unsafe_allow_remote_access = true`.

## Environment variables

Shore reads these environment variables. API keys are read on demand by each provider.

| Variable | Used by | Purpose |
|---|---|---|
| `ANTHROPIC_API_KEY` | `[chat.anthropic.*]` | Anthropic API authentication |
| `OPENROUTER_API_KEY` | `[chat.openrouter.*]` | OpenRouter API authentication |
| `DEEPSEEK_API_KEY` | `[chat.deepseek.*]` | DeepSeek API authentication |
| `GEMINI_API_KEY` | `[chat.gemini.*]` | Google Gemini API authentication |
| `XAI_API_KEY` | `[chat.xai.*]` | xAI Grok API authentication |
| `ZAI_API_KEY` | `[chat.zhipuai.*]` | ZhipuAI API authentication |
| `TAVILY_API_KEY` | `behavior.tool_use.search` | Web search backend |
| `SHORE_ADDR` | daemon + clients | Bind / target address; overrides config file, overridden by `--addr` |
| `SHORE_CHARACTER` | CLI / TUI | Default character to talk to; overridden by `--character` |
| `XDG_CONFIG_HOME` | startup | Where Shore looks for `~/.config/shore/` (standard XDG) |
| `XDG_DATA_HOME` | startup | Where Shore stores persistent data (standard XDG) |

Individual providers may support additional env vars for per-model overrides — see [`examples/config.toml`](../examples/config.toml) for the full list.

## `[daemon]`

<!-- written in Task 4 -->

## `[defaults]`

<!-- written in Task 4 -->

## `[behavior.autonomy]`

<!-- written in Task 5 -->

## `[behavior.tool_use]`

<!-- written in Task 6 -->

## `[memory]`

<!-- written in Task 6 -->

## `[chat]`

<!-- written in Task 7 -->

## `[advanced]`

<!-- written in Task 7 -->

## `client.toml`

<!-- written in Task 7 -->
