## verdict

1. **Resolved — concept provenance.** The updated packet quotes the applicable direct-shaping exception from new-work step 3 and records why this fixed-purpose native extension uses it. It explicitly states that no concept roll or decision-page lock occurred. This resolves the request for an applicable exemption without inventing a passed seed gate.
2. **Resolved — unsaved close protection.** `fix-unsaved-close.png` and `fix-close-compact.png` visibly preserve the editor and present reachable Keep editing / Discard & close actions, with Keep editing focused. `fix-kept-draft.png` shows the image and candidate colors retained after cancellation. Source routes Close and Ctrl+Q through the draft decision and installs the same decision in `on_window_should_close`; revision tracking and its regression test cover edits made during saving. The packet reports runtime exercise of Ctrl+Q, Keep editing and explicit discard. The native window-close callback was inspected in source, not exercised through a Fulcrum close gesture; this verdict makes no such runtime claim.
3. **Resolved — switcher focus versus selection.** `fix-switcher-selected-focus.png` shows a dashed focus outline on the active theme. `fix-switcher-unselected-focus.png` shows that dashed outline on another theme while the active theme retains its solid outline and fill. Source applies the dashed treatment only to keyboard focus.
4. **Resolved — missing-image recovery.** `fix-image-error.png` and `fix-image-error-compact.png` show the complete instruction, “Image not found. Check the path, then choose Extract colors again.” The loaded image and candidate palette remain visible, and the message fits both window sizes.

## remaining

Clear. All seven supplied correction captures were opened and are valid. No material regression introduced by this correction batch was observed. The ship disposition covers the scored fixes, not the whole surface. Full desktop validation remains separate and was still running in the supplied packet.

disposition: ship
