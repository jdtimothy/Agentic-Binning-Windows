# Brief for an Android session: a video's action times

For an agent working in `jdtimothy/Agentic-Binning-Android`. Written 11 November 2026 from the Windows work on the branch `claude/action-times-by-motion` (commit `a4cc8b6`, `jdtimothy/Agentic-Binning-Windows`).

## Prompt to start the session with

> Read `CLAUDE.md` and `Instructions.md` first. Then read `docs/notes/android-action-times-brief.md` in the Windows repository (`jdtimothy/Agentic-Binning-Windows`, branch `claude/action-times-by-motion`) and follow it. Step 1 is a check on the phone that decides whether anything gets built: do it, show me the action times you measured, and wait for my answer before step 2. Work on a new branch and open a pull request; do not push to `main`. Never make a paid Claude call.

## The problem, as seen on Windows

The vision model (LFM3-VL-2B, Q4_0) is asked for 2 to 5 actions with `m:ss` times. On Windows every clip got times bunched at its start: a 73-second clip, 16 frames about 4.6 s apart, got 0:00, 0:04, 0:09, 0:13 and 0:18. Those are exactly the times of its first five frames. The descriptions do mention the end of the clip, so the model sees every frame; it does not use their times.

**Android may not have this.** Android splices each time stamp next to its frame (`PromptAssembly.videoMediaText`). On Windows every frame goes ahead of the text, so the times could only be listed. The Windows test of "Android's layout" was therefore not a true copy of it.

Android's own test would not catch it: `GenieXVisionModelTest.video_frames_interleaveWithTimestamps` checks only that each time looks like `m:ss`.

## Step 1: find out whether Android has it (no code change)

1. On the phone, analyze three real clips of 30 seconds or more in which something visibly happens late (a person leaves, a wave breaks, the camera turns to something new).
2. For each, write down the clip's length, its frame times (`VideoTiming.sampleTimesMs`) and the times of the actions in the reply.
3. Android has the problem if the action times are the first few frame times in order, or if none falls in the second half of a clip whose second half has the event.
4. Report the table to the owner and stop. If the times are right, nothing below is needed except "Worth checking either way".

## Step 2: the fix, if step 1 shows the problem

What Windows does now, and what was learned getting there. `crates/core/src/analysis.rs` (`prompt`, `moments`), `media.rs` (`frame_changes`) and `pipeline.rs` (`moment_actions`) in the Windows repository are the reference.

1. **The main call writes no actions.** Description, tags, name and shot type only. In the grammar the value is fixed text, `"\"actions\": []"`, with no free white space before it. With `ws` allowed there, the model wrote blank space up to the 520-token limit on busy clips and the item failed with a cut-off reply.
2. **The app picks the moments.** Compare each prepared frame with the one before: mean difference per colour channel between 32 px copies, leaving out pixels that are black in both (the letterbox bars). Compare by colour, not brightness: a red square turning blue hardly changes in brightness. If the largest change is under 8 (0 to 255), nothing happened and the clip gets no actions. Otherwise take frame 0 as the opening, split the remaining frames into up to four equal stretches, and from each take its most-changed frame if that change is at least 8 and at least 40% of the clip's largest.
3. **One short call per moment.** System prompt: one short phrase of at most 14 words, only what is visible, subject then action. The call gets the single frame at the moment and no other context. Limit 40 tokens. Grammar: a capital letter, then text, ending at the first full stop.
4. **The time is the frame's own**, as `m:ss` to the nearest second.
5. **Drop repeats.** A phrase that shares half or more of its longer words with an earlier one is left out. If only the opening is left, the clip gets no actions.
6. **Bump the prompt version** and record why. Windows went from `analysis-v2-cli` to `analysis-v3-cli`.

Three things that were tried and made it worse; do not repeat them:

- Sending the frame before the moment as well: the model described the earlier frame.
- Sending the clip's description as context: the model repeated the description's ending for a frame half a second in.
- Ending the phrase at a line break instead of a full stop: the model never wrote one and ran to the token limit with markers such as `</think>` in the text.

On Android this is another `generate`-style call on the model that is already loaded (`GenieXVisionModel` resets and makes a fresh single-turn call each time), with one image. The context loaded for 16 frames already covers it. No database change: actions are stored as now.

## What it cost and gave on Windows

On 14 real clips (3 s to 73 s): none failed, the actions were spread over each clip (the 73-second clip: 0:00, 0:41, 1:08), and 5 still or steadily moving clips got none. Total analysis time was about the same as before (about 200 s against 215 s): the main call is shorter without the actions, and each short call took 0.5 to 0.9 s. Measure both on the phone; its speeds differ.

A phrase is only as right as the model's reading of one frame. It called bread being broken "peeling a potato".

## Worth checking either way

- **The white space before `actions`.** Android's photo grammar is `"[" ws "]"` after `"\"actions\":" ws`. Photos have not shown the blank-space loop, but it is the same shape that failed on Windows for video once actions were forced empty.
- **Still clips.** Android's video grammar forces 2 to 5 actions. A locked-off shot of a tree then gets filler such as "The camera continues its forward motion". Allowing none is a product decision for the owner.

## Rules that apply (from the Android `CLAUDE.md`)

- Read `Instructions.md` before touching GenieX code; its gotchas list is the review list for any model-facing change.
- `main` is protected: a branch and a pull request, never a push to `main` or a tag.
- Keep LFM3-1.7B selected for sorting and never make a paid Claude call without the owner.
- Changing prompt text or the grammar needs the prompt version bumped, and the unit tests that pin the prompt and grammar updated with a note saying why.
- A model-facing change is not done until it has been checked on the phone.
