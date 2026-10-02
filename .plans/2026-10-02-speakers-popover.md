# Speakers popover in the web UI and renaming confirmed speakers

Date: 2026-10-02. Restores the header entry point and the "rename at any time" rule
of [`2026-09-28-inline-speaker-assignment.md`](2026-09-28-inline-speaker-assignment.md)
(Decision 4, finding S2) in the web UI of
[`2026-09-29-macos-webview-ui.md`](2026-09-29-macos-webview-ui.md). Neither plan is
superseded; this one records what the web port dropped and how it comes back.

## What was wrong

The web port of the main window (WP2, commit 18d09a9) mounted the speaker picker only
on unconfirmed turns of the transcript (`transcript-tab.tsx`, `assignment !== "confirmed"`)
and turned the header's Speakers row into a static avatar stack. The "Confirm speaker"
button hides once every speaker is confirmed. Together: once a speaker is named, nothing
in the UI opens a picker for them again, although `MeetingStore.confirm` handles the
reassignment (previous person's voice recomputed) and `SpeakerOptions.build` excludes
only the speaker's current person. The SwiftUI version the port replaced offered the
picker on every speaker, in the transcript and in a header popover.

## Decisions

1. **The header's avatar stack and names are one button** (`speakers-trigger`, ghost,
   chevron) that opens the Speakers popover, modelled on Jamie's header. Nothing opens it
   automatically; "Confirm speaker" keeps opening the transcript picker on the first
   unconfirmed speaker.
2. **Speakers popover** (`apps/macos/web/src/windows/main/speakers-popover.tsx`): title
   "Speakers", one row per speaker in the snapshot's cluster order, never regrouped on a
   rename: avatar, the name as a button (`speaker-picker-<id>`), the person's email in
   faint when known, the "Suggested" / "Who is this?" badge on unconfirmed rows, a Play
   button (`speaker-play-<id>`) when the clip exists. Clicking a name expands the picker
   field and option list inline under the row, one row at a time; a pick collapses it and
   the popover stays open so the next speaker can be named.
3. **One picker body, two hosts.** `SpeakerPickerPanel` (field, `speakers.options`
   requests with the host's prefill, `speakers.select` on pick) is the body; the transcript
   wraps it in a popover (`SpeakerPicker`), the Speakers popover renders it inline. No
   nested popovers.
4. **Every speaker opens the picker in the transcript**, confirmed or not. The badge stays
   gated on unconfirmed. The host's `prefill` is nil for confirmed speakers, so the field
   opens empty and the option list omits the current person, as the picker always did.
5. **The bridge speaker carries `email`** (`MeetingDetailSnapshot.Speaker.email: String?`,
   zod `email: z.string().optional()`), the confirmed or suggested person's email from
   the export. Fixtures re-recorded with `STENO_RECORD_FIXTURES=1`.

## Out of scope

- Marking a speaker as unknown from the picker (the bridge still rejects `kind: unknown`).
- An excerpt under unconfirmed rows (the plan of 2026-09-28 had one; Jamie's popover does
  not, and the transcript already shows the words).
- Merging two clusters from the popover: naming two speakers the same still merges inside
  `MeetingStore.confirm`.
