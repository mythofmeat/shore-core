You are doing an implementation pass on a Godot 4.6 chat client called Shore GUI. Two analysis documents have been written: a creative proposal and a grounded UX review. Your job is to synthesize them into a coherent set of changes and implement them.

## Step 1: Read everything

Read in this order:
1. shore-gui/VISION.md — the north star
2. shore-gui/IDEAS.md — existing roadmap
3. shore-gui/CREATIVE_DRAFT.md — creative/experimental proposals
4. shore-gui/UX_IMPROVEMENTS.md — grounded UX analysis
5. All scripts in shore-gui/scripts/
6. All shaders in shore-gui/shaders/
7. shore-gui/scenes/main.tscn and config_panel.tscn
8. shore-gui/project.godot

## Step 2: Plan your implementation

Synthesize both documents into a prioritized implementation plan. Prioritize by:
1. Things that fix active usability problems (from UX doc)
2. Things that dramatically improve atmosphere with low effort (addons, shader swaps)
3. Creative features that are achievable and high-impact
4. Polish and refinement of existing systems
5. Experimental features that might be great or might need to be reverted

Write your plan before you start coding. Limit yourself to what you can do well in one session — it's better to ship 5 polished changes than 15 half-baked ones.

## Step 3: Install addons if needed

Godot addons are installed by copying files into shore-gui/addons/. You can:
- Clone repos and copy the relevant addon folder
- Download specific files from GitHub

Notable addons to consider (verify these still exist and are Godot 4.x compatible):
- PostFX (post-processing: vignette, grain, CRT, chromatic aberration, glitch) — slider-based, no shader code needed
- Anima (UI animation library, 89 built-in animations, CSS-like syntax)
- GodotSynth (procedural audio synthesis — oscillators, filters, effects)
- Color Correction and Screen Effects (per-tonal-range color grading)
- Godot Shaders Library (in-editor browser for godotshaders.com)

Research and verify before installing. Check GitHub stars, last update date, and Godot version compatibility.

## Step 4: Implement

Work in phases of max 5 files each. After each phase, verify your changes parse correctly:
- GDScript: check for type inference issues (Godot 4.6 requires explicit types in many cases)
- Shaders: ensure uniform types match what GDScript sets
- Scene references: ensure any new nodes or scripts are properly connected in .tscn files

Key constraints:
- The chat functionality is load-bearing. Never break message sending, receiving, streaming, or scrolling.
- Every effect must remain independently toggleable.
- The app must work with all effects disabled.
- Text must always be readable. No effect should reduce contrast or obscure messages.
- The Rust bridge (shore-gui/rust/) handles daemon communication — don't modify it unless necessary.

When you add or modify shaders, ensure the corresponding GDScript manager properly initializes all uniforms.

## Step 5: Document what you did

After implementation, update:
- shore-gui/IDEAS.md — mark implemented items, add new ideas that emerged
- shore-gui/VISION.md — only if the project direction evolved

Create a brief CHANGELOG entry (append to shore-gui/CREATIVE_DRAFT.md or a new file) describing what was actually implemented vs. what was proposed, and why you made the choices you did.

## Important notes

- You are free to be weird and experimental, but craftsmanship matters. A bad shader is worse than no shader.
- If an addon doesn't work or is too complex to integrate, fall back to hand-rolling.
- Prefer stealing individual shader code from godotshaders.com over taking addon dependencies for single effects.
- Test your mental model: if you write a shader with 8 uniforms, make sure the GDScript that drives it sets all 8.
- GDScript in Godot 4.6 is strict about type inference. Use explicit type annotations for variables assigned from expressions (especially division, comparisons, and method calls that return Variant).
