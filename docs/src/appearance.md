---
title: Appearance and Visual Customization - Zed
description: Customize Zed's themes, fonts, icons, UI density, and other visual settings to match your preferences.
---

# Appearance

Customize Zed's visual appearance to match your preferences. This guide covers themes, fonts, icons, and other visual settings.

For information on how the settings system works, see [All Settings](./reference/all-settings.md).

## Customize Zed in 5 Minutes

Here's how to make Zed feel like home:

1. **Choose an icon theme**: Run {#action icon_theme_selector::Toggle} from the command palette.
2. **Set your font**: Open Settings with {#kb zed::OpenSettings} and search for `buffer_font_family`.
3. **Adjust font size**: Change `buffer_font_size` and `ui_font_size` in Settings.

## Themes

ZedStorm uses the compiled **Islands Dark** palette. To change the colours during development, edit `assets/themes/islands/islands.json` and rebuild. Theme settings and OS appearance changes do not replace the palette.

See [Themes](./themes.md) for details.

## Icon Themes

Customize file and folder icons in the Project Panel and tabs. Browse available icon themes with the Icon Theme Selector ({#action icon_theme_selector::Toggle} in the command palette).

Icon themes support separate light and dark variants:

```json [settings]
{
  "icon_theme": {
    "mode": "system",
    "light": "Zed (Default)",
    "dark": "Zed (Default)"
  }
}
```

→ [Icon Themes documentation](./icon-themes.md)

## Fonts

Zed uses three font settings and their fallback counterparts for different contexts:

| Setting                   | Used for                  |
| ------------------------- | ------------------------- |
| `buffer_font_family`      | Editor text               |
| `buffer_font_fallbacks`   | Editor text               |
| `ui_font_family`          | Interface elements        |
| `ui_font_fallbacks`       | Interface elements        |
| `terminal.font_family`    | [Terminal](./terminal.md) |
| `terminal.font_fallbacks` | [Terminal](./terminal.md) |

Example configuration:

```json [settings]
{
  "buffer_font_family": "JetBrains Mono",
  "buffer_font_fallbacks": ["Nerd Font"],
  "buffer_font_size": 14,
  "ui_font_family": "Inter",
  "ui_font_fallbacks": ["Nerd Font"],
  "ui_font_size": 16,
  "terminal": {
    "font_family": "JetBrains Mono",
    "font_fallbacks": ["Nerd Font"],
    "font_size": 14
  }
}
```

### Font Ligatures

To disable font ligatures:

```json [settings]
{
  "buffer_font_features": {
    "calt": false
  }
}
```

### Line Height

Adjust line spacing with `buffer_line_height`:

- `"comfortable"` — 1.618 ratio (default)
- `"standard"` — 1.3 ratio
- `{ "custom": 1.5 }` — Custom ratio

## UI Elements

Zed provides extensive control over UI elements including:

- **Tab bar** — Show/hide, navigation buttons, file icons, git status
- **Status bar** — Language selector, cursor position, line endings
- **Scrollbar** — Visibility, git diff indicators, search results
- **Minimap** — Code overview display
- **Gutter** — Line numbers, fold indicators, breakpoints
- **Panels** — Project Panel, Terminal, Agent Panel sizing and docking

→ [Visual Customization documentation](./visual-customization.md) for all UI element settings

## What's Next

- [All Settings](./reference/all-settings.md) — Complete settings reference
- [Key bindings](./key-bindings.md) — Customize keyboard shortcuts
- [Vim Mode](./vim.md) — Enable modal editing
