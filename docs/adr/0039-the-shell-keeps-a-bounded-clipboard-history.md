# 0039: The shell keeps a bounded clipboard history

Proposed on November 27, 2026, for #367.

## Decision

Fulcrum captures text selections for the shell's clipboard history, even
when an app keeps the keyboard. The shell opts in on its existing,
UID-checked control connection. Only that connection owns the capture
lifetime. Disconnecting it clears history and stops capture. Lock clears
history and cancels a pending read; unlocking does not recapture the old
selection. Nothing is written to disk.

The history holds at most 50 items, 8 KiB per item, and 64 KiB in total.
Empty, invalid UTF-8, and oversized selections are skipped, not truncated.
Consecutive duplicate text is stored once. Fulcrum reads one selection at
a time through a nonblocking pipe, with a two-second deadline. Replacing
the selection cancels the previous read. It accepts UTF-8 plain text,
`UTF8_STRING`, or `text/plain`; it does not capture primary selection,
images, files, or drag data.

Only the shell control channel can read or clear history. The Agent and
unlock authority receive neither its contents nor its change events.
Contents never enter snapshots or logs. Clear also cancels pending capture.
The shell holds a copy only while its history overlay is open and drops it
on close or lock. Choosing an item copies it through ordinary Wayland
clipboard ownership, without synthesizing a paste into an app.

Super+V toggles clipboard history; Super+. toggles a searchable character
picker. Fulcrum emits these events only while unlocked. Both use the
shell's exclusive overlay, as the launcher does, and restore the earlier
keyboard focus when closed. The character catalogue is curated, local,
and needs no network service or new dependency.

## Why

Ordinary Wayland offers follow keyboard focus. Polling them in the shell
would miss copies made while another app has focus and could block the UI
on an unresponsive source. Fulcrum already owns selection lifetimes and
lock state, so it enforces bounded capture and clearing in the same event
loop. This adds no general clipboard-read authority for the Agent.
