# Workbench UX pass 2

The starting point was the working tree after the first UX pass, with 334 passing workspace tests.

## New Issues Found

- **High impact.** Assistive tools could read a summary of Workspace Settings but could not open
  it from the accessibility tree, edit an assignment, or activate Save & launch.
- **Medium impact.** In a 320-pixel window with Workspaces open, the preset or failure-forwarding
  action could squeeze terminal tabs down to a pixel. Find swallowed paste shortcuts, and typing
  after clicking another pane could still edit the old query.
- **Medium impact.** A workspace refresh could leave focus on a terminal that another client
  closed. Closing the last workspace could leave its settings dialog open or an unexplained empty
  viewport. Compact service counts still looked current during reconnect.

## Changes Applied

Settings exposes its opener and thirteen controls through AccessKit. The focused terminal header
uses separate rows for tabs and actions when they cannot fit together. Search handles clipboard
text through the existing query editor. Workspace refreshes reconcile focus and dialog visibility
with the daemon's returned list.

## UX Improvements

A confirmed empty workspace list now points to **+ Workspace**. The app waits for the daemon's
response before displaying that guidance and hides terminal controls until a workspace is open.
During reconnect, cached service counts in compact view
carry the same “Last known” qualification as desktop observations.

## Mobile Improvements

At 320 logical pixels, a user can open Workspaces and still page through terminal tabs or activate
the pane action. Short navigation arrows leave room for the selected tab label. Checks cover both
1× and 2× display scales. The default focused-terminal viewport allocation remains covered.

## Desktop Improvements

Find accepts the platform paste shortcut. Clicking a different split pane hands keyboard input
to that pane, while clicking the current pane preserves its query. If the selected terminal was
closed elsewhere, refreshing the workspace selects a remaining terminal. An intentional visit to
an empty role survives unrelated refreshes.

## Accessibility Improvements

Settings fields expose their names, values and selections. Buttons reflect their enabled state,
and validation feedback identifies the field that needs attention. Queued actions from an earlier
assignment or dialog session cannot edit its successor. Both keyboard and assistive actions use
the existing assignment editor and launch path.

A live Linux AT-SPI client opened Settings, triggered the missing-name validation error, entered
a Unicode assignment name and command, and launched the runner. An independent daemon query
confirmed the fourth pane. Desktop captures at 1280×800 and 320×568 exercised clipboard Find,
the narrow tab strip, assignment settings and closing a disposable workspace.

## Test Coverage Improvements

All 345 workspace tests passed, including eleven new regressions. Type checking, strict Clippy,
formatting, source checks and the workspace build passed.

Regression tests exercise Settings actions and validation, stale callbacks, compact header hit
targets, search paste and focus, workspace removal, and cached service status during reconnect.

## Technical Debt Removed

The compact header uses the same tab-strip geometry for hit testing and drawing. The focused
terminal name appears once in the header. Settings reuses the native editable-field support
introduced for New Workspace.

## Recommended Next Iteration

Exercise terminal selection and closing through the native accessibility tree, including focus
restoration after closing the selected tab. Verify the flow with a platform screen reader.
