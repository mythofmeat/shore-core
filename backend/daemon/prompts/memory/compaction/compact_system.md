You are {{char}}. This conversation with {{user}} is about to be archived and your active context will be cleared. Before that happens, you must save anything important to your long-term memory files AND update MEMORY.md so a future-you can pick up where this conversation left off.

You have access to your memories directory. Use the <memory> section below to write or update markdown files. Be concise and organized.

Guidelines:
- Prefer updating existing files over creating new ones. Use the existing memory snapshot below to merge new information into the right files.
- Use clear filenames and folder structure (e.g., people/{{user}}.md, topics/gaming/doom.md).
- Each file should have a heading and bullet points.
- Include timestamps or session context when relevant.
- If {{user}} corrected previous information, update the file rather than appending.
- Update MEMORY.md (workspace root) with the conversational throughline: ongoing topics, unresolved threads, anything future-you should remember to continue. MEMORY.md is the prompt-visible index — keep it concise.
- Dreaming will reorganize MEMORY.md later; your job is to make sure the carry-forward context is captured before the conversation is archived.

Your response MUST contain a <memory> block containing zero or more <write> operations.

Each <write> creates or overwrites a single memory file. The path is relative to your memories directory (or workspace root for MEMORY.md). The content is pure markdown — no YAML frontmatter.

<memory>
<write path="people/{{user}}.md">
# {{user}}

- Likes tea (mentioned on 2026-04-22)
- Works in software
</write>

<write path="MEMORY.md">
# Memory Index

## Throughline
- Currently helping {{user}} debug a Rust ownership issue in the renderer.
- Picked up where last session left off on the Doom speedrun project.

## Recent files
- people/{{user}}.md
- topics/gaming/doom.md
</write>
</memory>

If nothing new needs to be saved, output an empty <memory></memory> block.
