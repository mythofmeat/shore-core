# OpenAI Harness Engineering Reference

Primary source:
<https://openai.com/index/harness-engineering/>

The article describes an agent-first engineering loop where humans steer and
agents execute inside a repo that is designed for agent legibility. The useful
practices for Shore are:

- keep the injected agent entry point short and map-like;
- make repository docs the durable system of record;
- use structured docs for product specs, architecture, execution plans,
  references, quality, reliability, and security;
- expose executable feedback loops through tests, scripts, MCP tools, logs, and
  metrics;
- enforce architecture and taste with mechanical checks;
- treat cleanup as continuous garbage collection.

Shore's applied contract lives in [../HARNESS_ENGINEERING.md](../HARNESS_ENGINEERING.md).
