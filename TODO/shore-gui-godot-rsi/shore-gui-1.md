You are doing a creative analysis pass on a Godot 4.6 chat client called Shore GUI. Your job is to explore the codebase, understand what exists, and then write a creative/experimental feature proposal document. You are NOT implementing anything — just thinking and writing.

## Step 1: Read the project vision and existing ideas

Read these files carefully:
- shore-gui-godot/VISION.md — the project's aesthetic philosophy and intent
- shore-gui-godot/IDEAS.md — existing feature ideas and design principles

## Step 2: Explore the codebase

Read through the scripts, shaders, and scenes to understand what's currently built:
- shore-gui-godot/scripts/ — all .gd files (effects_manager.gd is the central hub)
- shore-gui-godot/shaders/ — all .gdshader files
- shore-gui-godot/scenes/main.tscn — the scene tree structure
- shore-gui-godot/project.godot — project configuration

Understand the architecture: how effects are toggled, how presets work, how the config panel connects to the effects manager, how the Rust bridge works.

## Step 3: Research available Godot addons

Search the web for Godot 4.x addons/plugins that could enhance this project. Specifically look for:
- PostFX / post-processing addons (vignette, grain, CRT, chromatic aberration)
- Shader libraries (godotshaders.com has a searchable collection)
- UI animation libraries (Anima is a strong option)
- Procedural audio tools (GodotSynth, GDSiON)
- Color grading / screen effect addons
- Anything else that fits the vision

Addons are installed by copying files into shore-gui-godot/addons/ — no system permissions needed.

## Step 4: Write the creative proposal

Create a file called shore-gui-godot/CREATIVE_DRAFT.md with your proposals. Structure it as:

### New Feature Ideas
Bold, experimental, weird ideas that align with the vision. Think: what would make someone say "wait, this is a CHAT APP?" Push boundaries. Be specific — describe what the user sees/hears/feels, not just abstract concepts.

### Existing Feature Improvements
For each existing system (rain shader, glass cracks, condensation, seagulls, starfield, CRT, phosphor text, etc.), propose specific improvements. Reference godotshaders.com shaders or addons that could replace or enhance what's there.

### Addon Recommendations
Which addons should be installed, and what they enable. Include installation steps (git URLs, which folders to copy).

### Mood/Preset Ideas
New preset combinations that create distinct atmospheres. Describe what each feels like.

### Ambient Detail Ideas
Small, subtle things that happen in the background. The kind of details you notice on the third day of using the app.

Be specific and concrete. "Better rain" is useless. "Replace the rain_fog.gdshader with the godotshaders.com 'Rain on Glass' SDF shader which generates realistic droplets with refraction, and layer it over the existing fog system" is useful.

Be weird and experimental. The vision says "aesthetic silliness" — lean into that. But every idea must pass the bar: "would this hold up as a detail in a polished indie game?"
