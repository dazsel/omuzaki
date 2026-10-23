---
title: "Who drives each character, window modes and starting points"
summary: "drivers per character, window modes and starting points in the studio"
area: design
---

# Who drives each character, window modes and starting points

The studio is one set of building blocks, not a set of modes per use case. A person streaming as themselves, an AI
companion on the desktop, a person co-streaming with an AI character and an AI-only stream are the same pieces mixed
differently:

| Mix | Characters and what drives them | Window |
|---|---|---|
| Stream as yourself | one character: webcam (face, head), microphone (mouth) | capture, for OBS |
| AI companion | one character: conversation (face, mouth, head) | on the desktop |
| Co-stream with an AI | two: you by webcam and microphone, the AI by conversation | capture, for OBS |
| AI-only stream | one or more: conversation, with chat events as its input | capture, for OBS |
| You with AI help | one: webcam on face and head, an agent on reactions | capture, for OBS |
| Puppeteer, rehearse, replay | any: a script or a recorded take | any |

This page settles the studio's side: where a person sees and changes who drives a character, how the window mode is
picked, and how the common mixes are offered without becoming features of their own. The drivers themselves (the one
interface for anything that moves a character), the live control ops and the conversation driver are App core's and
Tracking's to build; the window op and its modes are Broadcast's, and the windows themselves are the studio's. The contract the UI needs from them is in section 5.

## 1. Rules

1. **Nothing here is a use case.** No screen, setting or op is named after a companion, a co-stream or an AI stream.
   The mixes in the table are starting points (section 4) that set ordinary settings, and everything they set can be
   changed one by one afterwards (P5, P6).
2. **Who drives a character is a setting of its place in the scene** (the cast member, `scenes/<id>/cast/<member>`),
   not of the character: the same character can be yours in one scene and the AI's in another.
3. **Every change is an op** (P8): the inspector, the palette, an agent over MCP and a script set drivers the same way,
   and each change is in History with who made it, undoable.
4. **The UI draws the driver settings from their op's schema.** The inspector already edits any op through its schema,
   so a new kind of driver appears in the studio without UI code; the only drawing of our own is the status chip.
5. **A driver that can't run says why and what to do,** in the row where it was chosen ("No camera found. Plug one in
   or pick another."), and the character falls back to its rest and life layer rather than freezing.

## 2. "Driven by" in the inspector

Selecting a cast member (in the outliner under its scene, or by clicking it on the stage) shows a **Driven by** section
at the top of the inspector, above position and variant.

- **Compact by default.** One row: `Driven by  [Webcam and microphone ▾]  ● Face found · 60 fps`. The choices are the
  drivers that can run here, from `driver.list`, plus **Nobody** (rest and life layer only) and **Per part…**.
- **Per part, on demand.** **Per part…** opens one row per part the drivers move: Face, Mouth, Head, Body, Hands. Each
  row has its own driver dropdown and status chip. A part left on **Same as face** follows the face row, so the common
  case stays one choice.
- **Several drivers on one part.** A row's **Add** puts a second driver under the first, with a weight slider. Order is
  the bus's own layering (rest, score, tracker, audio, trigger: `omuzaki-anim-api` `blend.rs`), shown as written, never
  reorderable in the UI: a person can see why the hotkey wins over the webcam, not change the physics of it.
- **Status chips** use the design system's `StatusChip` with a word, never a dot alone: "Running", "Waiting for a
  voice", "No camera found", "Agent paused".
- **Agents.** An agent that wants to drive a character asks through the same op; while the session lacks the grant, it
  is an approval card ("claude-code wants to drive Kit's face and mouth"), as for any other op it can't run alone.

The Channels panel (Perform workspace) already shows, per channel, which source owns it. With several characters on
stage it gains a picker of the cast members at its top (a segmented control up to five, a dropdown beyond), and each
owner chip names the driver as well as the source: "Conversation · score", "Webcam · tracker". The expression strip acts
on the member chosen there.

## 3. The window mode

The Live workspace's output card gets one segmented control, **Window**: **In the studio**, **For capture** and **On the
desktop**.

- **In the studio:** the stage shows only in the studio's own viewport.
- **For capture:** today's clean output window (`output_window`): borderless, exact size, transparent where the
  platform allows it, for OBS's window capture.
- **On the desktop:** the character alone on the desktop: transparent, always on top, kept out of the taskbar,
  click-through on transparent pixels, dragged by the character itself (#758, #759). A small handle on hover offers
  size, opacity and **Back to the studio**; Esc works while it has focus.

Each choice runs Broadcast's window op, so an agent can switch it too. A platform that can't do a mode keeps it in the
control, disabled, with the reason as its tooltip ("Click-through needs a compositor on this Linux session").

## 4. Starting points

The common mixes are offered as **starting points**, in the first-run flow's Character step and in File > New, never
as a mode the studio stays in. Each is a card with a picture and one line:

| Card | Line | What it sets |
|---|---|---|
| Stream as yourself | "Your face and voice move your character" | webcam and microphone driving the character; window for capture |
| A character on your desktop | "Talk to it, and it talks back" | conversation driving the character; window on the desktop |
| Stream with an AI co-host | "You and an AI character side by side" | two cast members, one per driver set; window for capture |
| An AI that streams | "It reads chat and answers on stream" | conversation with chat events as its input; window for capture |
| Start blank | "Set everything yourself" | nothing |

Choosing a card runs its ops in one transaction, so a single undo takes it all back, and History names it ("Started
from Stream with an AI co-host"). A starting point is data, registered next to what it uses (a new `register_preset!`,
ADR 0054), so another crate or a plugin can add one without editing this list. A card whose drivers can't run on this
machine shows what it needs ("Needs a speech model: add one in Settings > Models") and still applies, so the person can
fix the missing piece afterwards.

## 5. What the UI needs from the driver contract

These are the UI's needs, for App core and Tracking to shape as they see fit; names are placeholders until they settle
them on #2.

1. **A list of the drivers that can run now:** for each, an id, its kind, a short name for people, the parts it can move
   (face, mouth, head, body, hands), and its status in words with a fix. The dropdowns read this.
2. **One op to set a cast member's drivers,** per part, with weights, whose schema the inspector draws:
   `cast.set_drivers { scene, member, part, drivers: [{ driver, weight }] }`. Dry-run, undo and approval as for any op.
3. **Status changes as events,** so a chip changes the frame it happens without polling.
4. **Ownership in the live frame per cast member:** the Channels panel already reads who owns each channel from the
   perform thread's mailbox; with several members it needs that per member, plus which driver the source came from.

Window modes need one thing from Broadcast: the window op takes a mode (`studio`, `capture`, `desktop`) and reports
which modes this platform can do, with the reason for each it can't.

## 6. Not here

- The drivers, the live control ops (set channels, play an expression, look at, play a clip, speak), voice services
  and the conversation driver: App core and Tracking.
- The window op and its modes: Broadcast. The desktop window itself (its surface, click-through and taskbar behaviour
  on each OS) is built in the studio's `aux_window`, beside the capture window (#758, #759).
- Which LLM or voice a conversation uses: the Models page (ML-13) and the conversation driver's own settings, drawn
  from their schema like everything else.
