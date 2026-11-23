# Workbench UX pass

Baseline commit `c50bdb7`. This pass covers the native workspace sidebar, terminal tabs, New
Workspace and assignment settings.

## New Issues Found

High impact: assistive tools saw New Workspace as one dialog and could not directly edit its
fields or activate its buttons. A blank pane looked like a connected terminal after its last
session closed. An exited terminal could still display an input cursor.

Medium impact: Ctrl+Tab visited workspaces in daemon order while the sidebar displayed repository
groups. At 320 logical pixels, opening the sidebar in focused mode gave navigation only 67 pixels.
Long folder paths clipped the edit position, and repository names with a shared prefix could hide
their distinguishing segment. Widget severity was conveyed by color alone.

## Changes Applied

The sidebar and workspace shortcuts now share their grouping order. Repository headings show the
shortest distinguishing suffix. Widget rows show their level in text. New Workspace exposes native
controls, and both setup dialogs preserve field labels and caret visibility when editing long text.

## UX Improvements

An initialized empty workspace now explains how to launch a terminal. During connection or startup,
the existing progress feedback takes precedence. Exited terminals retain their transcript and name,
show their exit code when available, and hide the input cursor.

## Mobile Improvements

At a 320-pixel native window width, explicitly opening Workspaces gives navigation its normal 240
logical pixels. Settings, paging and workspace creation stay reachable. Closing navigation restores
terminal space. A shorter form hint and an ellipsis on long workspace headings keep the narrow
header and dialog legible. The existing default layout still reserves two thirds of the viewport for
the focused terminal.

## Desktop Improvements

Ctrl+Tab and Ctrl+Shift+Tab move through the visible repository order in both directions and wrap
at the ends. Dialog buttons accept Space, and selected text has a visible replacement hint.

## Accessibility Improvements

The New Workspace tree exposes field names, values, selection, focus, button actions and validation
feedback. Requests from a previously closed dialog cannot change the current form. Assistive tools
receive terminal assignment names. A live Linux AT-SPI client edited the name in Japanese, read
the prefilled folder, and activated Cancel. Widget levels have a readable label alongside their
existing color.

## Test Coverage Improvements

Thirteen regression tests cover grouped navigation, narrow control targets at two display scales,
Unicode editing, native form actions, validation, empty and exited terminals, distinct repository
headings, and status levels. Existing contrast tests for all five themes and default viewport
allocation checks continue to cover the prior design.

The locked Linux workspace suite passed all 334 tests. Type checking, Clippy with warnings denied,
formatting, the workspace build and source checks passed. A desktop smoke test exercised the app
at 1280×800 and 320×568 and closed three terminals down to the empty state. Native accessibility
verification was performed on Linux.

## Technical Debt Removed

The renderer and keyboard navigation use the same repository grouping routine. Both setup dialogs
use the same caret-aware text formatter. The existing workbench design document was line-wrapped
to pass the repository Markdown check. Its words are unchanged.

## Recommended Next Iteration

Extend native accessibility actions to Workspace Settings. Test adding, editing and launching a
named assignment end to end with a screen reader, including focus after validation fails.
