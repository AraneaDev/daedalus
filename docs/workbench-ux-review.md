# Daedalus workbench UX review

This full review uses the workbench at commit `74927eb` as its baseline. It examines the
native desktop interface, narrow and short windows, keyboard operation, and accessibility.

## Issues Found

| Severity | Surface | Finding |
| --- | --- | --- |
| High | Agent presets and Action palette | Screen readers could read the dialogs but could not focus or activate individual choices. |
| High | Terminal search | The accessibility tree described the search but omitted controls for previous match, next match, and case sensitivity. |
| Medium | Workspace header | The Workspaces label clipped beside the workspace actions. Settings and Run tests had less visible button boundaries than the layout control. |
| Medium | Widget editor and import review | Adjacent field values started at different positions. Read-only import rows looked like disabled input fields. |
| Medium | Compact workbench | At 320 logical pixels, Cargo buttons could consume the space for all pinned widgets. At 180 logical pixels tall, a repository heading could appear without its workspace card. |
| Medium | Terminal states | The focused pane could remain blank while its workspace terminals started. Empty-state recovery guidance was missing from the accessible status. |

## Improvements Applied

- Exposed preset and palette choices as native buttons, kept accessible focus aligned with the
  highlighted row, and revealed palette choices when they received focus. Added Search controls
  that use the same actions and bounds as the pointer interface.
- Added complete compact workspace-disclosure labels and centered the workspace actions on
  consistent button surfaces. The full workspace caption remains available when space permits.
- Aligned editor values in a shared column. Import details now use readable text and separators.
  Primary Settings actions have consistent emphasis.
- Kept a pinned widget or its overflow control reachable beside Cargo actions at narrow widths.
  Short grouped sidebars compact the workspace card instead of showing only a heading.
- Added a visible and announced startup state for the focused pane. The same status provides
  recovery guidance for a workbench with no workspaces or no terminal in the focused role.

Existing split and focus views, terminal shortcuts, and the pane roles supplied usable
desktop navigation. The review reused that navigation and the existing theme.

## Metrics

- Reviewed seven live Linux desktop captures at 1000 × 570, including split and focused
  workbenches, workspace settings, environment setup, new worktree, failure details, and image
  preview. Also checked the widget editor and workspace import review at 1440 × 900.
- Verified 320-pixel width and 180-pixel height cases in layout tests at both 1× and 2×.
- All 717 workspace tests passed, including 24 accessibility tests. The separately run Linux
  desktop smoke test exercised the native accessibility tree.
- Formatting, source checks, strict Clippy, documentation, and the workspace build passed.
- Workspace coverage measured 94.00% of lines, 94.09% of functions, and 93.33% of regions.
  Existing mutation gates caught all 24 viable mutants across protocol framing and terminal erase.

## Remaining Opportunities

- Review a dedicated workspace drawer for explicitly opened sidebars at 320 logical pixels.
  The current side-by-side layout leaves about 80 logical pixels for the terminal.
- Validate the new accessibility controls with Narrator and VoiceOver on their native platforms.
- Review the environment setup form at the smallest supported window heights.
