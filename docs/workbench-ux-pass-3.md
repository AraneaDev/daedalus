# Workbench UX pass 3

The previous pass ended with 345 passing workspace tests. This iteration covers terminal
navigation, manual workspace creation, and command and agent pickers.

## New Issues Found

- **High impact.** The accessibility tree exposed the current terminal but offered no way to
  select another terminal or close a particular tab. Same-role terminals needed distinct targets.
- **High impact.** New Workspace dismissed the draft when the request was queued. A refused folder
  forced the user to enter the name and path again.
- **Medium impact.** In a 320-pixel window, picker instructions and long recipient names clipped.
  Held submission keys could reach the terminal after a form closed. Moving between different
  chooser dialogs could carry a partial scroll gesture into the next one.

## Changes Applied

Added a native terminal tab list, pane-specific actions and close-focus handling. New Workspace
tracks requests through confirmation and shows pending, refusal and connection-loss feedback.
Picker headings separate the title from shortcuts, and recipient rows preserve their pane ID.

## UX Improvements

A failed creation keeps both fields and selects the folder for correction. A pending request can
be dismissed. Its eventual reply updates the workspace list without taking focus from a newer
draft. When a connection drops before confirmation, the form asks the user to check the list before
retrying. An empty action palette points to the workspace manifest.

## Mobile Improvements

Compact pickers keep shortcut instructions above the selectable rows. Long recipient names yield
space to the identifying pane ID. Missing-directory feedback explains how to correct the folder,
and other long errors use up to two lines in the dialog. The accessibility tree retains the
original error. Desktop captures verified the preset picker at 1280×800 and 320×568, plus a
320-pixel refusal dialog followed by successful creation using the corrected folder.

## Desktop Improvements

Closing the selected terminal in the tab strip selects its right-hand neighbor, or the previous
tab when closing the final tab in the strip. Closing a background tab preserves focus. Submission
and dismissal keys stay consumed until release. Each chooser tracks its own scroll gesture.

## Accessibility Improvements

The native tab list exposes terminal names, selection state and the selected terminal's close
button. Actions target pane IDs and are checked against the active workspace, selection, dialog
and connection as appropriate. Closing the last terminal makes Workspace Settings the accessible
focus target. A live Linux AT-SPI client selected two Runner tabs, closed the remaining terminals
in sequence and verified focus returned to Settings. A daemon query confirmed zero panes. Native
macOS and Windows accessibility were not exercised.

## Test Coverage Improvements

Regressions cover same-role tabs, stale actions, focus after close, named and exited labels, draft
retention and retry, compact picker text, long folder errors, held form keys and scroll isolation.
A daemon integration test submits a missing folder and then creates the corrected workspace.
The Linux workspace suite passed all 355 tests, up from 345. Type checking, strict Clippy,
formatting, source checks and the workspace build passed.

## Technical Debt Removed

Visible and accessible terminal tabs share their labels and ordinals. Building labels now takes
two linear passes over the panes. Previously each unnamed pane rescanned the workspace to determine
its ordinal. Pointer and assistive actions share the same selection and close handlers.

## Recommended Next Iteration

Exercise workspace switching through the native accessibility tree, including grouped worktrees
with similar names. Check that the selected workspace and terminal are announced after switching.
