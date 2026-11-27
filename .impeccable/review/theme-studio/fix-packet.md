# Theme Studio review corrections

Source: 3b08b64, on main 7a2423f. The original review remains unchanged.

1. Concept applicability: see the updated review-packet.md provenance section,
   quoting new-work step 3's direct-shaping exception for local extensions or
   precisely specified narrow requests. No roll or user lock is claimed.
2. Unsaved work: DraftState tracks revisions, including edits that occur while
   a save is in flight. Close, Ctrl+Q, and the native window-close callback
   route to Keep editing / Discard & close. Escape keeps editing. Native
   testing exercised Ctrl+Q, Keep editing, and explicit discard. The native
   callback was code-inspected; no Fulcrum close gesture is claimed.
3. Switcher focus: keyboard focus has a dashed border; the active choice keeps
   its fill and solid border when not focused. Both selected and unselected
   focus were recaptured.
4. Missing image: the error explains how to correct the path and retry Extract
   colors. The image, candidate palette, and draft survive. Ordinary and
   compact captures show the entire recovery instruction.

The parent opened all seven new fix-*.png captures. The close confirmation
and error remain reachable at 820 × 660. Keep editing preserves the image and
candidate colors; explicit discard closed the native window.

Regression validation: Theme Studio's two headless tests pass, including
revision/close protection and a save completing after later edits; all 17
image-enabled theme tests pass. The full Void build/test of shell and Theme
Studio passed. The desktop check now also asserts that Ctrl+Q leaves an
unsaved native window open, then dismisses the decision with Escape before
saving. The full desktop run is in progress; no passing result is claimed yet.

Please give a scoped verdict against the original findings, with each marked
resolved, partially resolved, or unresolved. Do not broaden this into a new
full-surface audit or imply unperformed input paths were runtime-tested.
