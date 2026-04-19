# Shore GUI — Design System

Reference: [`shoreline.html`](./shoreline.html)

## Thesis

*A transmission received through fog.* Late-night, coastal, half-literary. Sodium-vapor orange is the only warm color in the world; everything else is paper, ink, and damp. The UI should feel like a found document, not software.

---

## Palette

All tokens are defined as CSS variables on `:root` in `shoreline.html`.

| Token         | Hex / Value                    | Role                                                  |
| ------------- | ------------------------------ | ----------------------------------------------------- |
| `--bg`        | `#0c0908`                      | Warm near-black base. **Never** `#000` or neutral-950 |
| `--bg-elev`   | `#141010`                      | Raised surfaces: modal, palette, tool block bg        |
| `--ink`       | `#e8dfd6`                      | Primary text (aged paper)                             |
| `--ink-dim`   | `#a69a8f`                      | User messages, secondary text                         |
| `--ink-mute`  | `#6b5f54`                      | Timestamps, labels, muted prose                       |
| `--ink-ghost` | `#3e342d`                      | Placeholders, rules, tertiary marks                   |
| `--ember`     | `#e67e22`                      | *The only real color.* Sigil, focus, streaming, accents |
| `--ember-dim` | `#c2410c`                      | Reserved for image accents / deep emphasis            |
| `--ember-glow`| `rgba(230,126,34,0.18)`        | Hover/glow halos                                       |
| `--rule`      | `#1e1916`                      | Borders and separators (2% luminance step from bg)    |

**Rule:** no cool grays, no blue whites, no secondary accent colors. Everything that isn't ember is on the warm neutral axis.

---

## Typography

Three families, each with a semantic role.

| Family          | Role                                                     | When                                                    |
| --------------- | -------------------------------------------------------- | ------------------------------------------------------- |
| **Fraunces**    | The character's voice. Literary, slightly weird.         | Character message bodies, modal titles, section labels, thinking, time gaps, command descriptions |
| **Inter**       | The user's voice. Modern, functional.                    | User messages, body default, toggle labels              |
| **JetBrains Mono** | Machine output. Structural, terminal-adjacent.        | Timestamps, daemon addresses, tool calls, command names, status values |

**Rule:** the character speaks serif, the user speaks sans, the machine speaks mono. Never mix.

Body sizing: 15px baseline, 16px for character voice, 13px for thinking, 11–12px for technical strings.

---

## Glyph vocabulary

A small, consistent set. Each glyph means exactly one thing.

| Glyph | Name            | Meaning                                 | Color                    |
| ----- | --------------- | --------------------------------------- | ------------------------ |
| `✦`   | Sigil           | Character identity                      | Ember, breathing         |
| `✎`   | Thinking        | Inner voice, reasoning                  | Ember (55% opacity)      |
| `∿`   | Tool            | Machine action, drawn from memory/world | Ember, with glow         |
| `⟩`   | Prompt          | Composer input marker                   | Ember (60% opacity)      |
| `▸`   | Chevron         | Collapsed state (rotates 90° on open)   | Ink-mute                 |
| `◯`   | Ember cursor    | Active streaming position               | Ember, pulsing           |

---

## Components

### Message

Role is conveyed by alignment + typography, never by bubbles or background fills.

- **User**: right-aligned, `--ink-dim`, Inter 14.5px, upright (not italic). No name label. Timestamp right-aligned below.
- **Character**: left-aligned, `--ink`, Fraunces 16px with slight softness (`SOFT: 40`). Name line above (`✦ MAREN` in small-caps serif 11px letter-spaced). Timestamp below.

### Time gap

When a gap between messages is substantial, a centered italic Fraunces line with soft dot-gradient rules:
```
────  three hours pass  ────
```
Copy is literary (`three hours pass`), not mechanical (`3h 4m ago`).

### Thinking (collapsible, closed by default)

Inline block inside a character message. Collapsed form is a one-line button: `✎ she thinks for a moment ▸`. Expanded reveals Fraunces italic dim body with a subtle ember-tinted left border. Can appear **before** the main body (pre-thinking) or **between** paragraphs (interleaved thinking). Same structural component either way.

### Tool use (collapsible, closed by default)

Monospace block, ember-tinted left border, faint ember-tinted background. Header is always visible: `∿ memory_read · query: "..."` followed by `▸`. Clicking expands the result block, which is separated by a dashed rule.

The tool block is the one place where the aesthetic intentionally breaks toward "mechanical" — this is a machine action, not voice. Mono font does the work. Keep it compact.

### Image attachment

Inline in message flow. 1px ember-tinted border (18% opacity), 2px radius, soft shadow. Max-width 340px. No caption. User images align right within their message; character images sit in body flow.

### Streaming indicator

Two coordinated signals:
1. The character's sigil switches from slow ambient breath to fast dramatic pulse (`.sigil.streaming`).
2. A glowing ember-colored dot (`ember-cursor`) sits inline at the end of the streaming text. Removed from DOM when streaming completes.

Never use typing-dots (`• • •`) — too chat-app.

### Composer

Borderless textarea, transparent background. Ember `⟩` prompt glyph at the left. Small circular send button at right (ghost border by default, ember glow on hover). No hint text below — keyboard affordances are learned, not posted.

### Command palette

Floating panel above the composer, appears when input starts with `/` or `:`. `--bg-elev` surface, ember left-accent-border, subtle glow shadow. Rows have mono command name + italic Fraunces description. Selected row has ember left-border + ember-tinted name with glow. Escape clears the input and dismisses.

### Settings modal

Fixed gear icon top-right (`--ink-ghost` → ember on hover). Modal is a centered panel over a blurred backdrop. Structure: title (Fraunces 20, `SOFT: 60`), small-caps mono subtitle, then sections. Section labels are ember small-caps 10px with a 1px rule below. Rows are label + value or label + toggle. Toggles are 32×18 pill with ember fill when on.

---

## Motion

All motion is slow, warm, ember-based. No sharp transitions, no bounce curves.

| Animation     | Target              | Cycle | Purpose                           |
| ------------- | ------------------- | ----- | --------------------------------- |
| `ember`       | `.sigil`            | 4.5s  | Ambient breath on every character sigil. Opacity 0.62 ↔ 1.0 with glow radius shift. |
| `ember-fast`  | `.sigil.streaming`  | 1.4s  | Dramatic pulse while streaming. Opacity 0.5 ↔ 1.0 with strong glow surge. |
| `pulse`       | `.ember-cursor`     | 1.1s  | Streaming cursor. Scale 0.8 ↔ 1.15, opacity 0.4 ↔ 1.0. |
| `chevron`     | `.chevron`          | 180ms | Rotate 90° on collapsible open.   |
| `toggle`      | `.toggle::after`    | 180ms | Pill switch slide.                |

Transitions on interactive elements: 120–180ms `ease`. Never longer than 200ms for discrete state changes.

---

## Overlays

Three fixed overlays sit above content:

1. **Grain** — SVG fractal-noise texture, 9% opacity, `mix-blend-mode: overlay`. Every view looks slightly damp.
2. **Vignette** — radial gradient, transparent 50% → `rgba(0,0,0,0.55)` 100%. Darkens corners.
3. **Fog-top** — sticky 180px gradient at top of the message scroll area. Caps at 55% darkness. Older history fades as it scrolls into this zone — legibility is preserved at peak dimming.

All overlays `pointer-events: none`.

---

## Principles

### Do
- Use **asymmetry** to convey role (alignment + typography), not backgrounds.
- Let **typography** do the work of UI. The app should read like a document.
- Keep ember the **only real color**. Everything else is ink on paper.
- Default collapsibles (thinking, tool use) to **closed**. User opts into detail.
- Preserve **legibility** at every state, including maximum fog dimming and in image placeholders.
- Match font family to **semantic source** (serif=character, sans=user, mono=machine).
- Keep glyphs **small and unique**. Each means exactly one thing.

### Don't
- Use chat bubbles, rounded message containers, or background fills to distinguish roles.
- Use cool grays, blue-tinted whites, or any secondary accent color. No cyan focus rings, no red errors (errors are ember-dim).
- Put avatars-with-initials anywhere. The sigil is the identity.
- Show typing-dots. Use the ember cursor.
- Add helper-text hints under inputs ("⌘+Enter to send"). Shortcuts are discovered, not advertised.
- Add emoji. Use the glyph vocabulary above.
- Introduce new font families or weights without updating this doc.

---

## Open questions

Flagged for future iteration:

- **Tool use aesthetic**: currently mechanical (mono, structural). Could reframe as ritual (serif, "drawing from memory"). Kept mechanical for now so tool calls are visually distinct from prose.
- **Error states**: no visual yet. Likely a dim ember-dim rule with italic Fraunces, echoing the time-gap pattern.
- **Multiple characters**: character switcher lives in settings for now. If we go multi-character in one window, sigil color may need to vary per character (but stay warm — rust, copper, amber, never cool).
- **Image captions / alt text**: currently unhandled. If needed, use italic Fraunces dim, below the frame.
