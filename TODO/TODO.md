- [x] regression: we have somehow lost a previous interiority integration step, where when the user messaged the assistant after the assistant had any amount of interiority ticks, the interiority ticks would be included as a system message (or prepended to the user message when using anthropic sdk). the interiority integration was placed after the time-gap-awareness section. (root cause: recap injection in `trim_messages` was gated on a 30-min time-gap threshold, so on active days recaps were silently dropped even though the interiority prompt promises the character they'll "surface when the user next messages". Dropped the gate — recap injection now fires whenever `entries_in_range(prev_ts, cur_ts)` returns anything, independent of wall-clock gap. Time-gap *marker* stays threshold-gated. Does not fix a character that has stopped writing `<recap>` entirely — that's downstream of the broken contract.)

- [x] bug: setting an interiority model seems to not do anything at all. (warm-path interiority tick was cloning `last_request` from the main chat turn and never rewriting `model`/`api_key` — only the cold rebuild-from-disk fallback honored `defaults.interiority`. Added `apply_interiority_model_override` in `autonomy/manager.rs` that rebuilds the request via `LedgerClient::build_request` when a distinct interiority model is configured)

- [x] regression: switching the model in the CLI appears to do literally nothing at all. (persisted via `$XDG_RUNTIME_DIR/shore/active_model`, reapplied on every connect — mirrors the active-character pattern)

- [x] followup: restore dynamic completions for `shore model <TAB>` / `shore character <TAB>` (fish-first: `shore completions fish` now emits a `shore complete {models,characters}` footer; daemon routes list_models through the characterless dispatch path so completions work on multi-character configs)

- [x] bug: the models list now includes tool models and other non-chat models for no reason

- [x] bug: `shore log` does not show the character name. only `Assistant`

- [x] bug: compaction and collation seem to wait for me to message before actually firing after idle (root cause: `tick_character` in `autonomy/manager.rs` only set `compaction_pending = true` on idle triggers, relying on the handler's post-generation path to actually run compaction — so idle detection fired, but no work happened until the user next messaged. Added `execute_idle_compaction` that calls `memory::compaction::run_compaction` directly from the tick when the tick context has its LLM/config/notifier/registry deps wired, reloads the character engine, and resets the trigger flags. Unit-test contexts without deps still fall back to the pending flag path.)

- [ ] followup: character has essentially stopped writing `<recap>` entries. As of 2026-04-14, exactly one recap exists in `recaps.jsonl`, timestamped `2026-04-08T17:08:57+10:00` — nothing since. The prompt contract fix (injection on short gaps) is in place, but the character needs to be prompted/rewarded for writing recaps again. Investigate whether the interiority prompt is landing correctly, whether recaps are being extracted on every tick, and whether there's cache/model drift (interiority model setter fix may be relevant here too).

- [x] bug: shore usage is broken
```
eshen@meat ~/D/silvershore (main) [2]> shore usage --anomalies 
server error ["internal_error"]: Invalid column type Integer at index: 10, name: total_ms
error: Invalid column type Integer at index: 10, name: total_ms
```

- [x] tweak: `shore usage --call-type` should filter by call type without any other options. how are users supposed to know what call types there even are?
```
eshen@meat ~/D/silvershore (main)> shore usage --call-type 
error: a value is required for '--call-type <CALL_TYPE>' but none was supplied
```

- [ ] bug: when trying to send a message with a chatgpt model
```
error: InternalError - HTTP 400: {"error":{"message":"Provider returned error","code":400,"metadata":{"raw":"{\n \"error\": {\n \"message\":
  \"No tool call found for function call output with call_id tool_remember_image_C6qJvvjYhJ955ffYCXdw.\",\n \"type\":
  \"invalid_request_error\",\n \"param\": \"input\",\n \"code\": null\n
  }\n}","provider_name":"Azure","is_byok":false}},"user_id":"user_2lEcCR3C7yKCDzxeUclpcbk337W"}
```

- [ ] feature: implement an MCP server for debugging and programmatic use purposes.

- [ ] weird intermittent memory agent issues:
```
qifei

  ▶ memory
  │ {
  │   "request": "search for caco doom wad launcher project"
  │ }

  ◀ memory
  │ No relevant memories found.

  ▶ memory
  │ {
  │ "request": "save: ren has two side projects alongside shore: 1) \"caco\" - a doom WAD launcher/library manager with 324 commits over ~2
  │ months. 2) a TTS daemon written in Go that spawns multiple backends using python venvs and presents them as an openai-compatible API, plug
  │ and play, supports LAN remote use. 152 commits over roughly the same period."
  │ }

  ◀ memory
  │ The requested information regarding ren's side projects has been saved to the memory database.
  │
  │ **Summary of Updates:**
  │
  │ * **Caco (Doom Launcher/Library Manager):** Updated details to reflect that the project has reached 324 commits over approximately two
  │ months. This complements existing context (IDs 20260216_002033_1, 20260313_150559_1, et al.) which establishes Caco as a Doom-themed tool
  │ featuring a sophisticated GUI, vim-binding TUI, and integration with multiple data sources like IDGames and DoomWiki. (New entry:
  │ 20260415_002904_0)
  │ * **TTS Daemon:** Updated details regarding the TTS daemon. It is verified as being written in Go, acting as a wrapper that spawns multiple
  │ backends via Python virtual environments, and providing an OpenAI-compatible API for LAN remote access. It has 152 commits over approximately
  │ the same timeframe as Caco. This builds upon the prior successful implementation of local TTS for Qifei (ID: 20260329_210737_1) and the build
  │ artifacts located in `/home/eshen/go/` (ID: c_20260411_013527_6). (New entry: 20260415_002905_0)
```
