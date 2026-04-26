Goal

Clean up stale runtime and documentation leftovers from the main -> dev changes, especially around removed “send an existing attachment image by path” behavior. Uploaded images should still work for vision/history/UI, and generated images may still be sent via generate_image, but the model must no longer be prompted to remember, reuse, or send saved attachment paths.

Context

The previous generic attachment-sending behavior is intentionally removed. However, uploaded images still produce model-facing annotation text in shore-daemon/src/handler/images.rs:

- format_image_annotation(rel_path) currently includes:
  “[Attached image saved as: ...]”
  “If you want to reference this image again later, use the saved path above.”

ingest_images(...) pushes that annotation as ContentBlock::Text, and shore-daemon/src/handler/task.rs appends those content blocks before the actual user text. This means every uploaded image still tells the model about a saved path and suggests path reuse.

The current desired behavior:

- Users can attach/send images to Shore.
- Shore may persist uploaded image files internally under the character data directory for history/UI/replay.
- The model may receive the image bytes/blocks for vision.
- The conversation may retain ImageRef metadata internally.
- The model must not be shown filesystem paths for uploaded attachments.
- The model must not be told to remember image paths.
- The model must not be told it can send attachment-directory images back later.
- Do not remove generated-image support unless it is explicitly implicated. The generate_image tool and ServerMessage::SendImage path are still useful for generated images.

Suggested files/modules/symbols to inspect

Runtime image upload path:
- shore-daemon/src/handler/images.rs
  - format_image_annotation
  - ingest_images
  - embed_image_data
  - encode_image_block
- shore-daemon/src/handler/task.rs
  - handle_generation
  - build_llm_messages

Generated-image path, keep intact unless tests reveal stale generic-send behavior:
- shore-daemon/src/tools/images.rs
  - generate_image tool
- shore-daemon/src/engine/tools.rs
  - tool-loop handling for generate_image
  - ServerMessage::SendImage emission
- shore-protocol/src/server_msg.rs
  - SendImage protocol event
- shore-matrix/src/bot.rs
  - MatrixBot::send_image
- shore-matrix/src/bridge.rs
  - ResponseCollector buffering SendImage

Tool registry/docs consistency:
- shore-daemon/src/tools/mod.rs
  - all_tools
  - available_tools
  - ToolToggles usage
- shore-daemon/src/tools/workspace.rs
- shore-daemon/src/tools/history.rs
- shore-config/src/app.rs
  - ToolToggles
  - TtsConfig
  - EmbeddedConfig
- examples/config.toml

Top-level docs:
- README.md
- FEATURES.md
- CONFIGURATION.md
- ARCHITECTURE.md
- DECISIONS.md
- docs/dev-info/INVARIANTS.md
- docs/dev-info/QUIRKS.md
- CHANGELOG.md, only current/unreleased notes and links; do not rewrite historical entries unless they are actively misleading current docs.

Implementation requirements

1. Remove model-facing uploaded-image path instructions.

Preferred implementation:
- Remove format_image_annotation entirely if it becomes unused.
- In ingest_images(...), continue saving/copying uploaded images and returning Vec<ImageRef>.
- Stop pushing ContentBlock::Text that includes the saved path.
- Ideally return an empty Vec<ContentBlock> for image-only annotations.
- If a text marker is absolutely needed for logs, use a neutral/pathless marker such as “[Image attached]”, but prefer no annotation because the LLM already receives image blocks through ImageRef handling.

Make sure these strings no longer appear in runtime model-facing content:
- “Attached image saved as”
- “If you want to reference this image again later”
- “use the saved path above”

2. Preserve image upload/vision behavior.

Do not break:
- CLI/TUI/Matrix image upload into ClientMessage images/image_data.
- persistence of ImageRef on messages.
- embed_image_data for wire history/display.
- build_llm_messages / encode_image_block inclusion of images in LLM requests.
- max_image_size resize/caching behavior.

3. Preserve generated-image support.

Do not remove:
- generate_image tool.
- ServerMessage::SendImage protocol type.
- Matrix/TUI/CLI handling of SendImage for generated images.
- generated image persistence on assistant tool-loop messages.

But docs should distinguish this clearly:
- “generate_image can create and send a newly generated image”
- “uploaded attachment paths are internal and are not exposed as something the character should remember/reuse/send”

4. Add or update tests for the uploaded-image annotation removal.

Add focused tests in or near shore-daemon/src/handler/images.rs / handler tests:
- ingest_images with image_data saves the image and returns one ImageRef.
- ingest_images returns no model-facing text content blocks, or at minimum no block containing a path/reference instruction.
- legacy image_paths path also does not create model-facing path instructions.
- No returned ContentBlock::Text contains “Attached image saved as”, “reference this image”, “saved path”, or the absolute/relative attachment path.

If existing tests expect annotation text, update them to match the new behavior.

5. Audit and clean stale docs/config references from main -> dev changes.

Update current docs to match the actual dev runtime tool surface.

FEATURES.md:
- Replace the “Memory tools” table that lists memory_read, memory_write, memory_search, memory_list.
- Current LLM-facing memory-related surfaces are:
  - workspace tools read/write/edit/list_files/search, which can use memory/... when memory gates allow it
  - search_history for conversation transcript search
  - CLI/MCP natural-language memory query command surfaces if still present
- Remove “scratchpad tools” from the current tool list unless a scratchpad implementation still exists.
- Replace “image send/generate” with clearer wording like “image upload/vision and generated images via generate_image.”
- Do not imply the character can send existing uploaded attachments by path.

CONFIGURATION.md:
- Update [behavior.tool_use.tools] example to match current runtime:
  - memory = true
  - memory_read = true
  - memory_write = true
  - generate_image = true
  - web_search = true
  - fetch_url = true
  - check_time = true
  - roll_dice = true
  - activity_heatmap = true
  - read = true
  - write = true
  - edit = true
  - list_files = true
  - search = true
  - search_history = true
  - exec = true
- Remove send_image from config docs.
- Remove scratchpad_* toggles unless code still supports them.
- Do not present memory_search/memory_list as current tools. If kept for legacy compatibility in ToolToggles, document them only as legacy/no-op/backward-compatible keys, or omit them from user docs.
- Fix embedded Matrix example to match current config structs:
  - EmbeddedConfig has bind_address and port, not bind_addr = "127.0.0.1:6167".
- Fix TTS example to match current TtsConfig:
  - current fields appear to be enabled, host, port, voice.
  - remove provider/model/api_key_env/base_url from TTS docs unless supported elsewhere.

docs/dev-info/INVARIANTS.md:
- Remove or rewrite “Scratchpad Vs Memory” if scratchpad tools were removed.
- Replace any “memory_* tools” phrasing with current workspace-memory/search_history wording.

docs/dev-info/QUIRKS.md:
- Replace “Disabling memory does more than hide memory_* tools” with wording that matches current behavior:
  - memory=false blocks memory/... workspace paths, hides/disables history/memory-related read surfaces as appropriate, and hides exec unless memory read/write are fully enabled.
- Keep the underlying memory-gate warning, just make the names accurate.

README.md:
- Fix broken/stale current docs links.
- docs/INVARIANTS.md and docs/QUIRKS.md appear to have moved to docs/dev-info/INVARIANTS.md and docs/dev-info/QUIRKS.md.
- README and CHANGELOG reference docs/PATCH_NOTES_OPENCLAWIFY.md; verify whether that file exists. If not, either create it or remove/update the link.
- Keep README’s high-level “generate images” claim only if it refers to generate_image, not sending existing attachments.

CHANGELOG.md:
- Only update current/unreleased notes and broken current links.
- Do not rewrite historical version entries just because they mention old features that existed at the time.

6. Optional code cleanup, only if safe.

In shore-config/src/app.rs:
- Review ToolToggles helper methods and is_memory_tool_name.
- If legacy keys like memory_search/memory_list are intentionally tolerated for old configs, leave them and add comments/tests clarifying they are compatibility aliases/gates, not registered tools.
- If they are accidental, remove them only after updating tests and docs.
- Do not break config loading for existing users without a deliberate migration note.

Constraints

- Preserve daemon/client boundaries.
- Treat SWP protocol changes as serious. Do not remove ServerMessage::SendImage unless you also migrate generated-image clients and tests; this task should not require that.
- Do not expose server filesystem paths to the LLM for user-uploaded images.
- Do not break image uploads, image display, or vision.
- Do not remove generate_image.
- Keep docs aligned with the actual registered tools in shore-daemon/src/tools/mod.rs.

Risks and edge cases

- Image-only user messages: if no annotation text and no user text, ensure the LLM request still contains the image block and an acceptable text block/content shape for the provider.
- History display: removing annotation text may make logs cleaner, but ensure UI still shows attached image entries via ImageRef.
- Remote clients: image_data path should continue to work without shared filesystem assumptions.
- Legacy image_paths path should not reintroduce path instructions.
- Memory compaction: removing annotation text means compaction should not learn useless attachment paths. This is desired.
- Tests/golden protocol fixtures may still include SendImage; keep them if generated images still use SendImage.
- Docs may include historical changelog references to send_image; those are okay when clearly historical.

Validation steps

Run:

cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings

Then run targeted tests, if available or added:

cargo test -p shore-daemon image
cargo test -p shore-daemon ingest_images
cargo test -p shore-protocol golden_json
cargo test -p shore-matrix bridge

Manual verification:

1. Send/upload an image with text.
2. Confirm the daemon still saves/persists the image internally.
3. Confirm the LLM request includes the image block.
4. Confirm no model-facing text includes:
   - saved filesystem path
   - “Attached image saved as”
   - “reference this image again later”
   - “use the saved path”
5. Confirm the UI/log still displays the attached image.
6. Ask the model about the image; it should answer based on vision, not path metadata.
7. Trigger generate_image if configured and confirm generated-image sending still works.
