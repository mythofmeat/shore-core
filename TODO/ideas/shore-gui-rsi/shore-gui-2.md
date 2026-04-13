You are doing a grounded UI/UX analysis pass on a Godot 4.6 chat client called Shore GUI. A creative proposal has already been written. Your job is to read it alongside the codebase and write a complementary document focused on practical improvements to usability, code quality, and polish.

## Step 1: Read the project context

Read these files:
- shore-gui/VISION.md — project philosophy
- shore-gui/IDEAS.md — existing feature roadmap
- shore-gui/CREATIVE_DRAFT.md — the creative proposal from the previous pass

## Step 2: Explore the codebase thoroughly

Read all scripts, shaders, and scenes. Pay special attention to:
- How messages are rendered and scrolled (main.gd, main.tscn)
- How the config panel works (config_panel.gd, config_panel.tscn)
- How effects are layered and toggled (effects_manager.gd)
- Text rendering: RichTextLabel usage, BBCode effects (fade_in_effect.gd, phosphor_effect.gd, wobble_effect.gd)
- Input handling and responsiveness
- The Rust bridge integration (rust/src/bridge.rs)

## Step 3: Evaluate what exists critically

For every system, ask: "What would a senior, perfectionist dev reject in code review?" Look for:
- Janky or placeholder-quality visuals/audio
- Hardcoded values that should be configurable
- Effects that interfere with readability or usability
- Missing transitions or abrupt state changes
- Code that's tangled or hard to modify
- Performance concerns (shader complexity, unnecessary per-frame work)
- Accessibility issues (text size, contrast, color-only indicators)

## Step 4: Write the grounded improvement document

Create a file called shore-gui/UX_IMPROVEMENTS.md with:

### Critical Fixes
Things that actively hurt usability right now. Bugs, jank, readability issues.

### Polish Pass
Existing features that work but feel rough. Specific, actionable improvements — not "make it better" but "add a 200ms ease-out tween when the config panel opens instead of the instant show/hide."

### Code Quality
Structural improvements to the GDScript codebase. Duplicated logic, missing type hints, overly long functions, unclear naming, etc. Be specific about which files and which functions.

### Text & Chat UX
The core chat experience. Message rendering, streaming display, scroll behavior, input field, copy/paste, keyboard shortcuts. This is the load-bearing feature — it must be excellent.

### Config Panel UX
The settings experience. Slider feel, preset switching, visual feedback, organization.

### Integration with Creative Draft
Review the CREATIVE_DRAFT.md proposals and note:
- Which ones are practical to implement well
- Which ones would need significant scaffolding first
- Which ones conflict with usability
- Suggested implementation order

Keep everything concrete and actionable. Every suggestion should be something a developer can pick up and implement without further clarification.
