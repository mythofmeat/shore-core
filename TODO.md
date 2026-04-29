# OpenAI-compatible tool use with reasoning content

Fixed:

- DeepSeek and Moonshot now use `reasoning_content` as their reasoning replay
  field.
- OpenAI-compatible tool-loop continuations preserve Shore `thinking` blocks
  until the OpenAI adapter can project them into `reasoning` /
  `reasoning_content`.
- In-progress tool loops no longer use the Anthropic-only unsigned-thinking
  filter for OpenAI-compatible providers.

Validation is recorded in
`docs/exec-plans/completed/openai-compatible-reasoning-tool-use.md`.
