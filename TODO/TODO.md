- [ ] followup: character has essentially stopped writing `<recap>` entries. As of 2026-04-14, exactly one recap exists in `recaps.jsonl`, timestamped `2026-04-08T17:08:57+10:00` — nothing since. The prompt contract fix (injection on short gaps) is in place, but the character needs to be prompted/rewarded for writing recaps again. Investigate whether the interiority prompt is landing correctly, whether recaps are being extracted on every tick, and whether there's cache/model drift (interiority model setter fix may be relevant here too). Potential plan that require's claude's review (since it was drafted via a cheaper llm) is at `./interiority-wrap-up.md`

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

- [ ] update *ALL* documentation
  - [ ] the readme in particular is very out of date and not explanatory enough
  - [ ] explain what the features are, why they exist, and how to use them and configure them
