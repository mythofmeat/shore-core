- [ ] shore-web? [./ideas/shore-web.md]
- [ ] refactor [./review/]
- [ ] shore memory client [./ideas/shore-memory-client.md]
# shore-tui
- [ ] when sending a message, the way the output gets formatted while streaming makes very little sense:

```
You

  hi qifei.

qifei

  [anthropic/claude-4.6-opus-20260205 | in:327 out:137 cache:0 | 2044ms]

qifei

  ▶ memory ···

```

it looks like:
1. the assistant sent a blank message. and it includes some stats that are also completely incorrect. that is not what the in/out/cache of that message was.
2. the stats that it shows are never-to-be-seen-again. they appear nowhere else ever. they aren't necessary
3. that the memory section is a separate message, as though the assistant sent two messages.
