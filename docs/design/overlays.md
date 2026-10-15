---
title: "Overlays and widgets: native and web widgets, layout per scene, events, scopes, the draw path"
summary: "native and web widgets, layout per scene, events, scopes, the draw path and the compositing step"
area: design
---

# Overlays and widgets: native and web widgets, layout per scene, events, scopes, the draw path

**Status:** proposed (2026-10-15). **Features:** BR-13 (overlays), BR-20 (the widget system); the captions of BR-13
with ML-03; SE-06's label on the AI bubble.
**Change:** #2069, covering #1031. **Settles the contracts of:** #1032, #1033, #1034, #1035, #1036 and #1187.
**Spec:** v3 §6 (ops, scopes), §7 (project layout), §8 (runtime), §9 (crates), §10 (budgets), §16 (ML-03), §25
(BR-10, BR-11, BR-13, BR-14, BR-20, BR-21), §30 (PL-03), §31 (SE-04, SE-06), §39 (rules 1, 2, 3, 6).
**Builds on:** `docs/design/automation.md` (accepted with #1893: the show folder, `register_show_key!`, the event
envelope `LiveEvent` and its normalised kinds, `EventSink`, machine-local approvals, live actions with `Undo::None`,
`op.run`), `docs/design/render-api.md` (the fixed frame graph, `paint_texture`, premultiplied sRGB targets),
`docs/design/record.md` D9 (program and clean feeds; `omuzaki-stream-api`'s `Feed`), `docs/design/audio-api.md` §4.2
(mute is privacy, co-host-only) and §5.2 (the A/V delay), `docs/design/ai-performer.md` §8 (`ai.line`),
`docs/design/scenes.md` (the pattern for a folder-shaped document), `docs/design/l10n-ops.md` (`Text`, message ids),
ADR 0015 (registration linking), ADR 0054 (generated module lists, non-exhaustive contracts, registered keys), and
the sheet text of `omuzaki-stage` (`src/sheet/layout/{text,fallback}`: harfrust shaping, Noto fallback fonts pinned
by hash, bidi).

This page explains the decisions so that an implementer or a reviewer can disagree with a reason. Names in the code
blocks bind the contract tasks; doc comments are the implementer's. Every struct-like variant of a public enum below is
`#[non_exhaustive]` with a constructor, and every contract struct marked so is built through `new` and `with_*`
(the standing ruling of #1514, ADR 0054 §4); the blocks leave the attributes out to stay readable, and D17 lists them.

## 1. What this covers

| Asked for (#1031 item) | Where | Contract task |
|---|---|---|
| 1. Crates | D1 | |
| 2. Where layout lives; inheritance; copy and duplicate | D2, D3 | #1032 |
| 7. Palette-driven styles, the contrast rule | D4 | #1032 |
| The widget kind contract, `View`, the control service | D5, D6 | #1033 |
| 6. The event feed, privacy, moderation, disclosure | D7 | #1033, #1035 |
| 8. Alerts (and the chat box and the AI bubble) | D8 | #1035 |
| 9. Captions | D9 | #1187 |
| 3. The native draw path: text, fonts, emoji, emotes, right to left | D10 | #1036 |
| 3. The compositing step and the `omuzaki-render-api` ask | D11 | #1036, asks R1a and R1b |
| 4. Which outputs carry overlays | D12 | |
| 5. Web widgets | D13 | new |
| 10. Ops, scopes, agents; "no widget reads the camera or files without a scope" | D14 | #1034 |
| 11. Plugins (PL-03) | D15 | |
| 12. Limits and budgets, the bench | D16 | #1036 |
| ADR 0054 conventions | D17 | all |
| A worked example on disk; a diagram from a chat message and a transcript to pixels | §4, §2 | |
| The tasks confirmed or re-cut, and what the design unlocks | §6 | |

**Not covered here, on purpose:**
- **BR-10's adapters** (sign-in, emote fetching, platform clients). D7 names the three chat moderation kinds the chat
  box needs and the emote cache it reads; BR-10's design builds them.
- **BR-14's moderation itself** (blocklists, rate limits). The chat box takes a `ChatFilter` (D7); BR-14 implements it.
- **Scene switching** (ST-04). The overlay host is told the live scene each frame (D6); it never switches one.
- **The editor panel's look** (the Broadcast workspace, `docs/design/ux/shell.md` §3): its task designs it on these ops.
- **Caption translation models.** D9 fixes the slot; the caption translation research picks a model.

## 2. The shape of it

```
 producers (any thread)                                    omuzaki-live inbox (#777): schema check, dedup
 BR-10 adapters ── chat.message, follow, raid, … ──┐       ┌───────────────────────────────────────────────┐
 omuzaki-speech ── voice.transcript (feed-only) ───┼──────►│ EventSink::publish                            │
 omuzaki-performer ── ai.line (TrustedEventSink) ──┘       │   ├─► automation engine (graph kinds only)    │
                                                           │   └─► EventFeed subscribers (D7)              │
 mic ─► voice tap `captions` (raw, 16 kHz; silent when     └──────────────────────┬────────────────────────┘
        muted or co-host-only) ─► speech host: VAD, ASR                           │ bounded queue per subscriber
        ─► transcript_event: a private utterance gives None (D9)                  ▼
                                                           overlay thread (omuzaki-overlay host, D6), runner clock
 shows/<show>/overlay/** ── compile, hot reload ─────────► ChatFilter (BR-14) ─► ChatBox │ Captions │ Alerts │ …
 (document ops: overlay.add, .set, .set_layout, …)          WidgetLogic::on_event, step ─► View (revision)
 live ops: overlay.show, .hide, .alert … ─► OverlayControl   painter (D10): View ─► shaped text, emotes, surfaces
                                                            ─► premultiplied RGBA8 in the widget's own pixels
                                                            ─► mailbox (triple buffer per widget)
 render thread, every frame (omuzaki-stage frame loop)                     │
   render(program) ─► [copy_target(program, clean) if a clean feed runs] ─► OverlayFrame::prepare
   (uploads changed pictures with paint_texture, resolves rects, fills layers) ─► composite(program, layers)
   ─► outputs submit: Feed::Program reads `program`, Feed::Clean reads `clean` (D11, D12)
```

## 3. Decisions

### D1. Crates: the contract `omuzaki-overlay-api`, the implementation `omuzaki-overlay`

| Crate | Kind | Holds |
|---|---|---|
| `omuzaki-overlay-api` | API | ids; the overlay document types (`OverlayDef`, `OverlaySettings`, `WidgetDef`, `Layout`, `LayoutPatch`), `paths` and `check`; layout `resolve`; palette tokens and `contrast_ratio`; `WidgetKind`, `WidgetLogic`, `StepCx`, `View`, `register_widget_kind!`, `DynWidgetKind`; the services `OverlayControl`, `OverlayFrame`, `ChatFilter`, `TextMeasure`; limits. Depends on `omuzaki-core`, `omuzaki-ops-api`, `omuzaki-doc-api`, `omuzaki-l10n-api`, `omuzaki-live-api`, `omuzaki-render-api`, `omuzaki-stage-api` (`SceneId`), `omuzaki-store-api` (`ContentHash`) |
| `omuzaki-overlay` | Implementation | the host (D6), the native kinds (`src/kinds/`, one file each), the ops (`src/ops/`), the painter (`src/draw/`), the web widget server (`src/web/`), the `overlay` show key's registration and check |
| `omuzaki-overlay-mock` | Mock | `KindHarness` (one kind, scripted events and a `FakeClock`, the `View`s it produced), `FakeControl`, `FakeMeasure` (1 per character, 2 per CJK ideograph), synthetic event fixtures |

- **Why the split, against putting it in `omuzaki-live` as spec §9 says:** `omuzaki-live` is now the automation
  engine (#1175), and overlays run without a show running (captions and the chat box are on screen whether or not any
  automation is). A separate contract lets the render thread's caller (`omuzaki-stage`), the speech host, BR-14 and
  the editor depend on the overlay API without the engine, and `lint-deps` keeps a kind from reaching into the host.
- **Why not one crate per kind** (as `omuzaki-live-nodes` is separate from the engine): the kinds share the painter's
  types and the host's test harness, there are eight of them, not a hundred, and generated module lists (D17) already
  give one file per kind with no shared file to edit.
- **Native only** for the implementation (`wasm = false`, reason "native text and image work, a local web server").
  The API crate builds for `wasm32-unknown-unknown`: the web player may show a saved show's widgets later.
- **Wiring** (ADR 0015, ADR 0054 §6): `omuzaki-overlay` defines `link()` with `omuzaki_core::define_link!` and sets
  `registers = true`; it registers its kinds, its ops, its two event kinds (D8) and the `overlay` show key.
- **Spec §9's row** gains the two crates in the spec's own change (not this one; §7).

### D2. The document: the show's `overlay` key, with a layout per scene inside it

```
shows/<show>/overlay/settings.toml                 safe margins, palette source and colours, language, port
shows/<show>/overlay/widgets/<widget>.toml         kind, name, [layout] (the default), [style], [params]
shows/<show>/overlay/scenes/<scene>/<widget>.toml  a LayoutPatch: only the fields this scene changes
```

| Key | File | Holds |
|---|---|---|
| `overlay` (a registered show key, D21 of automation) | `shows/<id>/overlay/` | the folder |
| `settings` | `overlay/settings.toml` | `OverlaySettings`: `safe` (four fractions), `palette` (D4), `language`, `web_port` (D13) |
| `widgets` | `overlay/widgets/<widget>.toml` | one `WidgetDef` each |
| `scenes` | `overlay/scenes/<scene>/<widget>.toml` | one `LayoutPatch` per (scene, widget) that differs from the default |

- **Why the show, not the scene document** (#1031 item 2 offered a sibling key of the scene node, a `widgets` key of
  `SceneDef`, or the show): widgets are **broadcast setup**, and `docs/design/settings.md` D7 already puts overlays in
  the show beside outputs, hotkeys and redeems. A widget's state must also outlive a scene switch: the alert queue,
  the chat history and the caption lines are one instance each, whichever scene is live. With widgets defined per
  scene, switching from "main" to "just chatting" would drop queued alerts or need cross-scene identity rules. And it
  needs no change to `omuzaki-stage-api` (no cross-area edit of `SceneDef::SIBLING_KEYS`), a show template carries its
  overlays whole, and the scene stays a stage that any show can use.
- **"Layout per scene" is still literal:** each scene may move, resize, hide or restack each widget. The default is
  the widget's own `[layout]`; a scene file overrides only the fields it names. **Inheritance is one level**
  (default, then the scene's patch), so what a scene shows is always readable from two files.
- **The key is `overlay`,** registered by `omuzaki-overlay` with `register_show_key!` (automation D21 rule 1: the key is
  named after its owner crate). A binary that doesn't link overlays keeps the subtree byte for byte and shows it as
  "kept, not checked" (rule 2 and 3); only `omuzaki-overlay`'s check and ops read or edit it.
- **Why a patch file holds flat fields:** the store makes a node holding only objects a folder (`omuzaki-doc`
  rules 1 to 3). A patch is therefore flat (`anchor`, `offset`, `size`, `fit`, `safe`, `visible`, `z`), so it is
  always one file, and an empty patch is removed (no empty files, as for scenes).
- **Ids** are slugs (`WidgetId`; scene folders use the stage's `SceneId`). Names are display text. A patch for a
  scene that no longer exists is kept and reported (`overlay-layout-unknown-scene`, a finding), never dropped, because
  the scene may come back with a pull or an undo; `overlay.reset_layout` removes it.
- **Copy and duplicate:** `overlay.add {copy_of}` copies a widget's definition and every scene patch of it under a new
  id in one patch (a second chat box styled like the first). A copy of the show and a library apply copy the whole subtree
  (automation D21 rule 2). A scene duplicated by `stage.duplicate_scene` gets no overlay patches by itself (the stage
  doesn't know overlays); `overlay.copy_layout {from_scene, to_scene}` copies them, and the editor offers it after a
  duplicate.
- **Which show the overlays come from:** the running show (automation D5) if one runs, else the project's default
  show (automation D2). A project with no show has no widgets; `overlay.add` on a missing show is refused with the
  hint `show.create {id: "main"}`, as automation's ops are.
- **Runtime state is never in the document:** live visibility (`overlay.show` and `.hide`), queued alerts, chat lines,
  caption lines and web widget tokens. They change during a stream, must not travel in a shared show and must not be
  undo steps.

### D3. Layout: anchors, fractions of the output, safe margins, one exact `resolve`

```rust
// ── ids ─────────────────────────────────────────────────────────────────────────────────────────────
slug_id!(WidgetId);
/// `chat_box`, `alerts`, `captions`, `ai_bubble`, `goal`, `timer`, `now_playing`, `sponsor_card`, `web`,
/// or `plugin.<plugin>.<name>`: ^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$, at most 96 bytes.
pub struct WidgetKindId(Cow<'static, str>);

// ── layout ──────────────────────────────────────────────────────────────────────────────────────────
pub enum Anchor { TopLeft, Top, TopRight, Left, Centre, Right, BottomLeft, Bottom, BottomRight }   // serde snake_case
pub enum Fit { Box, Width }               // Box: `size` is width and height; Width: the height follows the content
pub struct Layout {
    pub anchor: Anchor,
    pub offset: [f32; 2],                 // fractions of the output, -1..=1
    pub size: [f32; 2],                   // fractions of the output, 0.01..=1; with Fit::Width the second is ignored
    pub fit: Fit,
    pub safe: bool,                       // place inside the safe rectangle
    pub visible: bool,                    // the document default; the live state can override it (D6)
    pub z: i16,                           // higher draws later; ties by widget id
}
pub struct LayoutPatch {                  // every field optional; `None` inherits the widget's Layout
    pub anchor: Option<Anchor>, pub offset: Option<[f32; 2]>, pub size: Option<[f32; 2]>, pub fit: Option<Fit>,
    pub safe: Option<bool>, pub visible: Option<bool>, pub z: Option<i16>,
}
impl Layout { pub fn patched(&self, patch: &LayoutPatch) -> Layout; }
/// Fractions of the output; default 0.05 each. Only `SafeMargins::new` builds one: each side finite and in 0..=0.4,
/// `left + right <= 0.8` and `top + bottom <= 0.8`, so the safe region is never empty or inverted.
pub struct SafeMargins { left: f32, top: f32, right: f32, bottom: f32 }
impl SafeMargins { pub fn new(left: f32, top: f32, right: f32, bottom: f32) -> Result<Self, OverlayError>; }
pub struct RectPx { pub x: i32, pub y: i32, pub w: u32, pub h: u32 }
/// `content`: the content's aspect (height over width) for `Fit::Width`, from the widget's last `View`.
pub fn resolve(layout: &Layout, output: UVec2, safe: SafeMargins, content_aspect: Option<f32>) -> RectPx;
```

**`resolve`, exactly** (the arithmetic of #1032, kept): the *region* is the safe rectangle (the output minus `safe`
times the output size) when `layout.safe`, else the whole output. The box is `size` times the output; for `Fit::Width`
its height is its width times `content_aspect`, or times 0.25 when `None`. It is placed so that its anchor point lies on
the region's matching point, `offset` times the output size is added, and the box is clamped inside the region (a box
larger than the region is first shrunk to it). Arithmetic in `f64`, `round` to whole pixels.
**Inputs are valid by construction:** `SafeMargins` comes only from `new` (above), and `check_overlay` refuses a
`Layout` or `LayoutPatch` with a non-finite number, an offset outside -1..=1 or a size outside 0.01..=1 before it is
stored (#1032's `check`), so `resolve` never sees an inverted region or a non-finite size; it `debug_assert!`s both.
The region is at least 20 % of each side, so a resolved box is never zero-sized. #1032's acceptance
numbers stand unchanged: at 1920 x 1080 with 5 % margins, `BottomRight`, `size [0.25, 0.1]`, `safe` gives
(1344, 918, 480, 108), and the rest of its table.

- **Why fractions of the output, not pixels:** one layout serves 1080p, 1440p and 4K outputs and the editor viewport at
  any size. Text size is a fraction too (`Style::text_size`, D4), so a widget scales as a whole.
- **Why anchors and offsets, not free x and y:** a widget at `BottomRight` stays in the corner when the margins or the
  output's aspect change (a 9:16 vertical output), which is what streamers expect from OBS's own alignment.
- **Why the clamp:** an agent or a typo can't put a widget off screen, so "no pixel outside the safe rectangle" (#1036)
  is a property of `resolve`, tested by proptest, not of the painter.
- **One program size.** The layout resolves against the program target's size; the editor viewport scales the
  composited program (D12). Several program sizes at once (horizontal and vertical streams together) is a follow-up
  (§6): it needs a layout per output aspect, which this shape allows as a second patch level later.

### D4. Styles: palette tokens, a show palette, and a contrast rule with findings

```rust
pub enum PaletteToken { Surface, SurfaceAlt, Text, TextMuted, Accent, Positive, Warning, Danger }   // serde snake_case
pub struct OverlayPalette(pub BTreeMap<PaletteToken, Colour>);   // every token present after resolution
impl Default for OverlayPalette { /* dark, high contrast: surface #101018e6, text #ffffff, accent #8c7cff, ... */ }
pub enum PaletteSource {
    Default,
    Character { character: Option<CharId>, slots: BTreeMap<PaletteToken, String> },   // None: the live scene's subject
}
pub struct PaletteSettings { pub source: PaletteSource, pub colours: BTreeMap<PaletteToken, Colour> }
pub fn resolve_palette(settings: &PaletteSettings, character: Option<&omuzaki_face_api::Palette>) -> OverlayPalette;
pub struct Style {
    pub surface: PaletteToken, pub text: PaletteToken, pub accent: PaletteToken,
    pub radius: f32,                      // corner radius, a fraction of the box's shorter side, 0..=0.5
    pub outline: bool,                    // a 2-pixel ring in the surface colour around glyphs, at full alpha
    pub text_size: f32,                   // body text height, a fraction of the output height, 0.01..=0.2 (0.025)
}
pub fn contrast_ratio(a: Colour, b: Colour) -> f32;   // WCAG 2.x relative luminance ratio, on opaque colours
/// The lowest ratio of `text` against `surface` composited over any backdrop: 1.0 when the text's luminance lies
/// between the surface over black and the surface over white, else the ratio to the nearer of those two.
pub fn worst_contrast(text: Colour, surface: Colour) -> f32;
```

- **Where tokens come from** (#1031 item 7): one palette per show, resolved in three layers, each overriding the last:
  the default palette; then, with `source = character`, the character's LK-03 palette slots that `slots` names (for
  example `accent = "hair"`, `surface_alt = "outfit"`; a token whose slot is missing keeps the default); then the
  explicit `colours`. **Why the show and not the scene:** one look per stream is what branding wants, and a scene
  changes where things are, not how they look. A per-scene palette can come later as one more optional patch file,
  additively.
- **Why a fixed token set** (eight tokens, closed by decision, not `#[non_exhaustive]`): it is what a widget's style
  can name and what the contrast check walks. Kinds never take raw colours, so a palette change restyles every widget
  and a single changed token changes only the widgets that use it (#1036's golden check).
- **Text tokens are opaque.** `text`, `text_muted`, `accent`, `positive`, `warning` and `danger` (every token any
  kind may draw text in) must have alpha 255; `check_overlay` refuses a palette (default, character-derived or
  explicit) that resolves one of them translucent, naming the token, so text is never see-through and the contrast
  rule below compares the colour actually drawn. Only `surface` and `surface_alt` may be translucent.
- **The contrast rule:** for each widget, `text` and `text_muted` against `surface` and `surface_alt` must reach 4.5:1
  (WCAG 2.x, normal text), and so must every extra token the kind draws text in (`WidgetKindInfo::text_tokens`: the
  chat box's role colours `accent`, `positive` and `warning`, D8); `accent` against `surface` must also reach 3:1
  as a non-text colour (WCAG 1.4.11). **A translucent surface is checked against every backdrop,** because the
  backdrop is the game or the camera and can be any colour: compositing is monotone in the backdrop, so the surface's
  luminance over any backdrop lies between its luminance over black and over white, and `worst_contrast` is 1:1 when
  the text's luminance falls inside that interval (a fully transparent surface always fails), else the ratio to the
  nearer end. With `outline` on, text is checked against the opaque surface colour, since the ring guarantees that
  edge whatever lies behind. A breach is a **finding** (`overlay-low-contrast`, naming the widget, the two tokens
  and the ratio), not an error: the person sees it and may keep a stylistic choice.
- **Chat name colours** (`ChatUser::colour`) are used when they pass 4.5:1 on the widget's surface, else the name takes
  the role's token (D8), so a dark-blue name never vanishes on a dark box.
- `contrast_ratio`'s acceptance numbers are #1032's: white on black 21.0; `#767676` on white 4.54; `#777777` on white
  4.48; white on `#202020` 16.29 (±0.01).

### D5. The widget kind contract: self-registered kinds, pure logic, an abstract `View`

```rust
// ── kind ────────────────────────────────────────────────────────────────────────────────────────────
pub struct WidgetKindInfo {
    pub id: WidgetKindId,
    pub summary: &'static str,                       // English; `widget-<kind>` (D17)
    pub consumes: &'static [&'static str],           // event kinds (automation D9 names), for the feed filter
    pub needs: Needs,
    pub hides_on_panic: bool,                        // BR-14's panic hides it (the AI bubble)
    pub content_fit: bool,                           // supports Fit::Width (its View reports a height)
    pub text_tokens: &'static [PaletteToken],        // tokens it draws text in beyond the style's (D4's check)
    pub cross_fade: bool,                            // keeps a second picture for `View::fade_out` (D5)
}
pub struct Needs { pub camera: bool, pub files: bool, pub network: bool, pub transcript: bool, pub viewer_data: bool }   // all false: none
pub trait WidgetKind: Send + Sync + 'static {
    type Params: Serialize + DeserializeOwned + JsonSchema + Default + Send + Sync + 'static;   // deny_unknown_fields
    fn info() -> WidgetKindInfo;
    fn check(params: &Self::Params) -> Result<(), ParamErrors> { Ok(()) }
    fn create(params: Self::Params, cx: &KindCx<'_>) -> Result<Box<dyn WidgetLogic>, KindError>;
}
pub struct KindCx<'a> { pub grants: &'a WidgetGrants, pub language: &'a Locale }
pub struct WidgetGrants { /* what the person approved for this show on this machine: camera, files (roots), network
                             (hosts), transcript; built by the host from the automation `Approvals` store */ }
pub enum WidgetNeed { Camera, Files, Network, Transcript, ViewerData }                        // what NeedsGrant names
pub enum KindError { NeedsGrant { need: WidgetNeed }, Params(ParamErrors) }      // ParamErrors: live-api's (node)
pub struct WidgetKindRegistration { /* fn pointers like automation's NodeRegistration: info, schema, defaults,
                                       check (normalised params), create */ }
#[macro_export] macro_rules! register_widget_kind { ($ty:ty) => { /* inventory submit + __record_registration */ } }
pub fn widget_kinds() -> impl Iterator<Item = &'static WidgetKindRegistration>;
pub fn find_widget_kind(id: &str) -> Option<&'static WidgetKindRegistration>;
pub trait DynWidgetKind: Send + Sync { /* the registration's functions as &self methods: plugins (D15) */ }

// ── logic ───────────────────────────────────────────────────────────────────────────────────────────
pub trait WidgetLogic: Send {
    /// An event of a kind this widget consumes, in arrival order.
    fn on_event(&mut self, ev: &LiveEvent, cx: &mut StepCx<'_>);
    /// A live command (`overlay.clear`, `overlay.alert`). A kind that doesn't take one returns `Unsupported`.
    fn on_command(&mut self, cmd: &Command, cx: &mut StepCx<'_>) -> Result<(), OverlayError>;
    /// Advances time and updates `view` in place. Never allocates once the view's buffers have grown; bumps
    /// `view.revision` only when what the view shows changed (alpha and slide are not content).
    fn step(&mut self, cx: &mut StepCx<'_>, view: &mut View);
}
pub struct StepCx<'a> {
    pub now: Seconds,                                // the runner clock; logic never reads a clock itself
    pub content_px: [f32; 2],                        // the widget's resolved box, minus padding, in output pixels
    pub measure: &'a dyn TextMeasure,
    notices: &'a mut dyn NoticeSink,                 // `cx.notice(kind, payload)`: the D8 events
}
pub trait TextMeasure: Send + Sync {
    /// The advance width in output pixels of `text` drawn at `style`: the painter's own shaping (D10).
    fn width(&self, text: &str, style: &MeasureStyle) -> f32;
}
pub struct MeasureStyle { pub px: f32, pub bold: bool, pub lang: Option<Locale> }   // Locale: omuzaki-l10n-api's
pub enum Command { Clear, Alert(AlertRequest) }
impl View {
    /// Every number finite; `alpha` and `fade_out` 0..=1 (`fade_out` only on a `cross_fade` kind); `slide` -1..=1; `content_aspect` 0.01..=100; image sizes and bar values
    /// 0..=1; at most `MAX_VIEW_ITEMS` (64) items, 512 spans and 4 KiB of plain text in all.
    pub fn validate(&self) -> Result<(), OverlayError>;
}

// ── view ────────────────────────────────────────────────────────────────────────────────────────────
pub struct View {
    pub items: Vec<ViewItem>,
    pub alpha: f32,                                  // whole-widget opacity, applied at composite: no repaint
    pub slide: [f32; 2],                             // offset as a fraction of the box, applied at composite
    pub fade_out: Option<f32>,                       // a cross-fade: the previous picture's opacity (0..=1)
    pub content_aspect: Option<f32>,                 // for Fit::Width
    pub revision: u64,                               // bumps when items change; one input of the paint key (D10)
}
pub enum ViewItem {
    Text { spans: Vec<Span>, lang: Option<Locale>, align: Align, size: TextSize, max_lines: u8 },
    Image { image: ImageRef, size: [f32; 2] },       // size: a fraction of the content box
    Bar { value: f32, label: Option<Content>, token: PaletteToken },
    Chip { label: Content, token: PaletteToken },    // e.g. the AI disclosure (SE-06)
    Gap(f32),                                        // a fraction of the text size
}
pub enum Span {
    Text { content: Content, token: PaletteToken, bold: bool },
    Emote { emote: EmoteRef, alt: Arc<str> },        // drawn as an image one line high; `alt` if the image is missing
}
pub enum Content { Message(Text), Plain(Arc<str>) }  // Message: our own words, resolved by the painter in
                                                     // OverlaySettings::language; Plain: a viewer's or performer's text
pub enum Align { Start, Centre, End }                // Start is left in left-to-right text, right in right-to-left
pub enum TextSize { Small, Body, Title }             // 0.8, 1.0 and 1.4 times Style::text_size
pub struct EmoteRef { pub platform: Platform, pub id: Arc<str> }
pub enum ImageRef { Asset(ContentHash), Emote(EmoteRef) }
```

- **`View` is retained content, not pixels** (as #1033 said): logic is tested without a GPU or a font, the painter is
  swappable, and the same `View` is what a web preview or a screen reader could read.
- **Why `Content::Message` and `Content::Plain`:** our own words ("Thanks for the raid!") are message ids with typed
  arguments, so they translate and pass the l10n check; a viewer's chat line or the performer's words are never run
  through Fluent. Resolving at paint time, not in `step`, keeps `step` free of formatting and allocation.
- **Cross-fades are two layers.** A kind that declares `WidgetKindInfo::cross_fade` (the AI bubble) bumps
  `revision` for the new content and sets `fade_out` to the outgoing content's opacity, lowering it to `None` over
  the fade. The host keeps the previous picture in a second texture for such kinds, and `prepare` pushes two layers
  for the widget while `fade_out` is set: the previous picture at `alpha × fade_out`, then the new one at
  `alpha × (1 − fade_out)` within the same clip. Tests: during a 0.15 s cross-fade stepped at 30 Hz the widget
  yields exactly two layers whose opacities sum to its alpha, and a mid-fade golden matches the reference composite
  of both pictures.
- **Why `alpha` and `slide` are outside `revision`:** fades and slide-ins change every frame; they are applied when
  compositing (D11), so an animation costs no repaint and no upload.
- **Why `StepCx` carries `content_px` and a `TextMeasure`:** captions must decide their own line breaks so a finished
  line never reflows (D9). The measure is the painter's own shaping, so logic and pixels agree; tests use
  `FakeMeasure`. A scene switch that resizes the box is the one moment lines may reflow.
- **Needs** (BR-20: "no widget reads the camera or files without a scope"): `create` refuses with
  `KindError::NeedsGrant` when `info().needs` asks for what `cx.grants` lacks; a kind with no needs always creates. A
  refused widget is **inert** with a finding and an approval card in the Broadcast workspace, as an unapproved
  automation node is (automation D11), and approvals live in the same machine-local store. No first-party native
  kind needs anything except `captions`, which needs `transcript` (D7), granted by default to first-party kinds
  only. `viewer_data` is for web packages only (D13): first-party native kinds hold viewer data in Omuzaki's
  memory under D7's rules and need no grant for it. **Camera** has no grant in v1 at all: no kind or web widget may read it, and a manifest that declares it is
  refused with that reason; a camera scope is an `omuzaki-ops-api` addition when a feature needs one.
- **What a kind can reach:** nothing but its `StepCx`. Images come by `ContentHash` from the project store or as emotes
  from the emote cache, both through the painter; a kind gets no file, socket or clock API, so the needs are enforced by
  construction, not by review.
- **Params** follow automation's node rules: a JSON Schema from the typed `Params`, `deny_unknown_fields`, checked at
  `overlay.add` and `.set`, refused with the path of the bad field.

### D6. The host: one overlay thread, a feed subscription, live state, the control service and the frame seam

```rust
pub trait OverlayControl: Send + Sync {
    fn show(&self, widget: &WidgetId) -> Result<(), OverlayError>;
    fn hide(&self, widget: &WidgetId) -> Result<(), OverlayError>;
    fn toggle(&self, widget: &WidgetId) -> Result<(), OverlayError>;
    fn clear(&self, widget: &WidgetId) -> Result<(), OverlayError>;
    fn alert(&self, alert: AlertRequest) -> Result<(), OverlayError>;
    fn set_panic(&self, on: bool);                                 // BR-14: hides `hides_on_panic` kinds
    fn live(&self) -> Vec<WidgetLive>;                              // per widget: kind, visible, inert, counters; no text
}
pub struct AlertRequest { pub widget: Option<WidgetId>, pub title: Option<String>, pub text: String,
                          pub kind: Option<String>, pub duration: Option<Seconds> }
pub struct WidgetLive { pub id: WidgetId, pub kind: WidgetKindId, pub visible: bool, pub state: LiveState,
                        pub counters: BTreeMap<&'static str, u64> }       // e.g. alerts `queued`, `deduped`, `dropped`
pub enum LiveState { Running, Inert { why: Text }, Faulted { why: Text } }
pub enum OverlayError { UnknownWidget { id: String, nearest: Option<String> }, Unsupported { kind: WidgetKindId },
                        NoAlertsWidget, Invalid { why: Text } }

/// The render thread's seam (`omuzaki-stage`'s frame loop calls it; `omuzaki-overlay` implements it).
pub trait OverlayFrame: Send {
    /// Uploads widget pictures that changed since the last call (`Renderer::paint_texture`), and fills `layers` for
    /// a target of `size` showing `scene`. Never blocks and never allocates once warm.
    fn prepare(&mut self, renderer: &mut dyn Renderer, size: UVec2, scene: &SceneId, layers: &mut Vec<Layer>)
        -> Result<(), RenderError>;
}
```

- **One overlay thread** owns every `WidgetLogic` of the current show. It wakes at the program's frame rate on the
  runner clock (and on an event), runs `on_event` for queued events in arrival order, `step`s every visible widget,
  and repaints widgets whose paint key changed (D10). It never runs on the render or audio thread and never takes the
  executor's lock. An idle overlay (nothing visible, no events) sleeps.
- **Why its own thread, not the automation engine's:** overlays run with no show running, the painter's work (shaping,
  rasterising) is not graph work, and a busy show must not delay captions.
- **Live visibility** is a runtime override per widget (`Shown`, `Hidden` or none). The effective visibility is the
  override, else the live scene's layout `visible`. The override survives scene switches and resets when the host
  reloads the show (project open, show change), never written to the document.
- **Hot reload** follows `Runner` patches under the show's `overlay` key, like the automation engine (D5 there): a
  widget whose kind and params are unchanged keeps its logic and state (a layout or style edit restyles without losing
  the chat history); a changed one is recreated.
- **Faults:** a panic in a kind is caught, the widget becomes faulted (hidden, a finding), the rest keep running.
  **Every `View` is validated** (`View::validate`) after each `step` that bumped its revision or moved its alpha or
  slide, before anything reaches the painter or the frame seam; an invalid view faults that widget the same way and
  its last picture is withdrawn. So a first-party bug or an untrusted plugin (D15) can only lose its own widget, and
  `prepare` never hands `composite` an opacity outside 0..=1 or a non-finite rectangle.
- **The frame seam:** `OverlayFrame::prepare` is the only thing the render thread calls. It takes the newest finished
  picture of each widget from a per-widget triple buffer (lock-free), uploads only changed ones, resolves each visible
  widget's `RectPx` for the target size and the live scene, and pushes one `Layer` per widget in (`z`, id) order with
  `opacity = view.alpha` and the slide applied. **The slid rectangle is clipped to the widget's region** (the safe
  rectangle when `layout.safe`, else the output): `dst` is intersected with the region and `src` trimmed to match, so
  a slide-in enters from the margin's edge and no pixel ever lands outside the safe rectangle mid-animation.
  `omuzaki-stage`'s frame loop then calls `Renderer::composite` (D11). #1036's alpha scan runs on every frame of an
  alert's slide-in and slide-out and of the bubble's cross-fade, not only on still frames.
- **Determinism:** logic is a pure function of its params, the events with their times and the step times; the
  `KindHarness` runs a script twice and compares the `View`s byte for byte (ADR 0009).

### D7. The event feed: which kinds, privacy, moderation, disclosure

| Kind (widget) | Consumes (automation D9 names) | Privacy and safety |
|---|---|---|
| `chat_box` | `chat.message`; `chat.delete`, `chat.clear_user`, `chat.clear` (new, asked of BR-10 below) | after `ChatFilter`; a deleted message leaves at once |
| `alerts` | `follow`, `sub`, `sub_gift`, `cheer`, `tip`, `redeem`, `raid` | names and amounts shown; nothing kept after display |
| `ai_bubble` | `ai.line` (`omuzaki-performer-api`'s kind and type) | the SE-06 chip on every non-empty view; hidden on panic |
| `captions` | `voice.transcript` (feed-only) | private utterances never published; needs `transcript` |
| `goal`, `timer`, `now_playing`, `sponsor_card` | their own designs (§6) | |

- **The feed.** The overlay host subscribes to accepted events (after the inbox's schema check and dedup, automation
  D9) through a new `omuzaki-live-api` service, `EventFeed` (ask L1): `subscribe(kinds, capacity, sink) ->
  Subscription`, where the sink is called on the inbox's thread and must not block; the host's sink pushes into its own
  bounded queue (droppable kinds shed first, as the inbox does). The feed runs whether or not a show is running.
  **Why not a second `EventSink`:** a fan-out in front of the inbox would duplicate the schema check and the dedup, so
  an EventSub redelivery would show an alert twice.
- **`voice.transcript` is feed-only** (ask L1: `EventKindRegistration::feed_only()`): the engine never triggers on it,
  never keeps it in its last-50 ring or the values monitor, and `on.event` can't name it. **Why:** automation keeps the
  person's words out of graphs on purpose (`voice.command` carries "never the transcript"); a graph `on.event →
  net.http` must not be able to post everything the person says. Only a subscriber that asks for the kind by name gets
  it, and the host gives it only to widgets whose `transcript` need is granted.
- **Private speech never becomes an event** (audio-api §4.2): a muted mic or `CohostOnly` makes the `captions` tap
  silent, so nothing is recognised for captions; when the speech host shares one recogniser with the performer's
  `listen` tap, an utterance heard in `CohostOnly` is marked private by the host and `transcript_event` returns
  `None` for it (D9). The event therefore has no `private` field: no consumer can mishandle a flag it never sees.
- **Chat only after moderation** (BR-14): the chat box calls `ChatFilter::allow(&ChatMessage) -> bool` before a
  message enters its state; BR-14 implements it (blocklists, link rules, first-time chatters held), and the host is
  given one as a service (allow all when no moderation service is linked, said on the Broadcast panel). The chat box's
  `delay_s` (default 0) holds each message that long before showing it, so a moderator's delete within the delay
  never reaches the stream. Deletes, timeouts and clears arrive as three kinds BR-10 publishes and #773's table gains
  (ask L1): `chat.delete {platform, channel, message_id}`, `chat.clear_user {platform, channel, user_id}`, `chat.clear
  {platform, channel}`; the chat box drops the matching lines at once, shown or held.
- **Viewer data is kept nowhere by Omuzaki** (SE-04): names, messages, avatars and amounts live only in the widget's memory for
  as long as it shows them (`max_messages`, `ttl_s`, the alert queue), are never written to a file, a log, the op log
  or `WidgetLive`, and every `Debug` impl over them prints lengths, not text. Emote images are cached by the emote
  cache under their platform id (public images, LRU, D16), never with who sent them. **Web widgets are the
  exception, said plainly:** their page is untrusted code running in OBS's browser, where Omuzaki can't stop it from
  writing what it receives to the browser's storage. So viewer data reaches a web widget only through the
  `viewer_data` need (D13), approved per show with a card that says so, and each load clears what the page stored
  before.
- **The AI disclosure** (SE-06): the AI bubble puts a `Chip` with the message `overlay-ai-disclosure` ("AI") in every
  non-empty view; params may restyle it, not remove it. `ai.line` comes through the performer's `TrustedEventSink`, and
  private turns publish nothing (ai-performer §8), so the bubble never shows a private exchange.
- **Overlay notices.** The host publishes two kinds of its own (registered by `omuzaki-overlay`, `Source::Overlay
  {widget}`, a new variant asked in L1): `overlay.alert_started {widget, alert_kind, test}` and `overlay.alert_ended
  {widget, alert_kind}`, so a graph can play a sound or fire a light when an alert shows (D8). They carry no viewer
  name or text.

### D8. Alerts, the chat box and the AI bubble: pure state machines

The behaviour, parameters and acceptance of #1035 stand; the design fixes what they hang on.

- **Alerts** (`alerts`): a queue ordered by priority, then arrival; one alert shows at a time for `duration_s`, then
  `gap_s`; the same kind from the same user within `dedupe_s` is dropped and counted; a full queue (`max_queue`, at
  most 200) drops the lowest priority, oldest first, and a newcomer of higher priority evicts that one; `paused` holds
  the queue; the priority table defaults to raid 5, sub gift 4, sub 3, cheer 3, tip 3, redeem 2, follow 1, and is a
  param. A **test alert** (`overlay.alert`, priority 9) goes to the front but never interrupts the one showing.
  `min_amount` per kind (bits, tip micros) filters small ones; a kind set to priority 0 is ignored. An
  `overlay.alert` may set its own duration, clamped to `min_display_s` (1.0) and `max_display_s` (30.0); a queued
  alert older than `max_wait_s` (120) is dropped and counted, so a long raid train never shows stale thanks minutes
  later.
- **Text:** each alert kind has a message (`overlay-alert-raid`: "{ $name } is raiding with { $viewers } viewers!")
  with typed arguments; the person may replace the message per kind with their own template (`templates.raid =
  "{from.name} brought {viewers} friends"`), whose placeholders are **dotted paths into the kind's payload** to a
  scalar leaf (`{viewers}`; `{from.name}` for `raid`, whose raider is `from: ChatUser` in automation D9;
  `{user.name}` for `follow`, `{gifter.name}` for `sub_gift`, `{reward.title}` for `redeem`). Paths are checked at
  `overlay.add` and `.set` against the kind's registered payload schema, so a misspelt or non-scalar path is refused
  with the nearest valid one; an optional field that is absent at run time (an anonymous gifter) renders as the
  message `overlay-alert-anonymous`. Values are substituted as plain text (never markup, never Fluent), so a viewer's
  name can't inject anything. The built-in messages take the same paths as their typed arguments.
- **Sounds and other effects are not the widget's** (#1031 item 8's soundboard hook): logic is pure, and effects go
  through ops. When an alert starts showing the host publishes `overlay.alert_started` (D7), and a graph `on.event →
  op.run(sound.play)` plays the sound (BR-21) in the show's session, with the show's grants and limits. The seed
  template "alert sounds" (one graph, a sound per alert kind) ships with the alerts kind's change, so a person without
  graphs gets sounds by applying it. **The other direction is the op:** `overlay.alert` is an op, so `op.run` makes it
  a graph action for free (#780) and agents call it like any tool.
- **Chat box** (`chat_box`): the newest `max_messages`, each leaving after `ttl_s`; text over `max_chars` cut with an
  ellipsis, keeping emote spans entirely inside the cut and dropping the others (emote offsets are byte offsets into
  `text`, automation D9); commands (`!…`) hidden when `hide_commands`; names in the user's colour when it passes 4.5:1
  on the box's surface, else coloured by the top role (each role token is in the kind's `text_tokens`, so D4
  checks it at 4.5:1 on that surface) (Broadcaster `accent`, Moderator `positive`, Vip `warning`, Subscriber
  `text`, others `text_muted`); `/me` lines in italic-free `text_muted`; a reply shows its parent's author only (never
  the parent's text). Plus `delay_s` and the moderation kinds (D7).
- **AI bubble** (`ai_bubble`): reveals the line at `cps` characters a second, or over `duration_s` when the event gives
  it; a new line cross-fades over `fade_s`; the bubble hides `linger_s` after the reveal; the disclosure chip (D7);
  hidden while panic is on.
- Every kind uses `Content::Message` for its own words and `Content::Plain` for others', so #1035's l10n check lists
  exactly the kind's message ids.

### D9. Captions: transcripts in, lines that never jump, timed to the voice viewers hear

```rust
// omuzaki-speech (#1187's speech half): registered with register_event_kind!(Transcript, feed_only)
pub struct Transcript { pub utterance: u64, pub text: String, pub is_final: bool, pub lang: String,
                        pub start: Seconds, pub end: Seconds, pub translated: Option<Translated> }
pub struct Translated { pub text: String, pub lang: String }
/// Partial and final transcripts of utterances that are not private; everything else (VAD, drops, errors, a private
/// utterance) gives None.
pub fn transcript_event(ev: &SpeechEvent, source: Source, at: Seconds) -> Option<LiveEvent>;

// omuzaki-overlay: kind `captions`
pub struct CaptionParams { pub max_lines: u8 /* 2 */, pub hold_base_s: f32 /* 3.0 */, pub hold_per_char_s: f32 /* 0.04 */,
                           pub hold_max_s: f32 /* 6.0 */, pub mode: CaptionMode /* original | translated | both */,
                           pub follow_av_delay: bool /* true */ }
```

- **Partials and finals** (#1187, kept): a partial replaces the current partial of its utterance; a final replaces its
  partials and starts its hold, `hold_base_s + hold_per_char_s × chars`, at most `hold_max_s`.
- **Line breaking** (the six languages): greedy to the box width from `StepCx::content_px` and the real `TextMeasure`,
  at `unicode-linebreak` (UAX #14) opportunities; spaces for English, Spanish and Portuguese; for Japanese, Korean
  and Chinese, a CJK run may break between any two ideographs or kana, Korean prefers spaces between words where
  present; **kinsoku:** no line starts with closing punctuation (`。、」』）〉！？` and the full-width comma and full
  stop) or a small kana, and no line ends with opening brackets; such a character pulls one character back with it.
- **No jumpy reflow from partials:** a line that is complete (another line follows it) never changes when a later
  *partial* of the same utterance arrives, in text or position; only the last line grows. A partial that revises
  words on a completed line changes only the open last line (the recogniser's revision shows once the final comes).
- **One bounded correction at the final:** the final is authoritative, so the utterance is laid out again from the
  final text once. Leading lines whose text is unchanged keep their exact position; from the first line that differs,
  the lines are replaced in place (same slots, no scroll), and any extra line is appended at the end. After the final
  nothing of that utterance changes again. #1187's tests gain: a final that corrects a word on the first of two
  completed lines replaces that line in place, keeps the lines before it, and changes nothing after its hold starts. Only the last `max_lines` lines show; older ones
  scroll off. Text carries its `lang`, so the painter picks the regional Han forms (D10).
- **Timing against the voice** (#1031 item 9, audio-api §5.2): captions are recognised from the `raw` tap, which leads
  the voice viewers hear by the A/V delay `D` when a changer is in use. With `follow_av_delay` (the default) a line
  appears no earlier than `start + D` on the runner clock, so a caption never leads the voice it captions. ASR's own
  latency (300 ms to 1 s for a final) is usually larger than `D`, so this mostly costs nothing; it matters for fast
  partials on an RVC-class changer (`D` = 200 ms). `D` comes from the perform settings through a read-only service the
  host is given: `omuzaki-overlay-api`'s `trait AvDelay: Send + Sync { fn current(&self) -> Seconds; }`, which the
  stage implements over the delay its frame loop already ramps (one atomic, written by the frame loop and read by the
  overlay thread) and hands to the host as part of ask R1b. Until R1b lands the host uses 0 and shows the finding
  `overlay-captions-not-delay-aligned` when the perform delay is above 0. Audio-api §5.2's "overlays and alerts are not delayed" still holds for
  everything except this alignment of captions to their own audio.
- **Latency from speech to pixels:** the speech host's part is ML-03's (VAD end-of-speech plus the recogniser); the
  overlay part, transcript published to the composited frame, is at most 50 ms p95 at 60 fps (D16).
- **Translation** (the slot): a transcript may carry `translated` (a later ML slot, picked by the caption translation
  research, fills it in the speech host); `mode` shows the original, the translation, or both (the original above in
  `TextSize::Small`, `text_muted`). The widget never calls a model.
- **Privacy:** D7. #1187's test "a private final never appears" becomes a speech-half test (`transcript_event` gives
  `None` for a private utterance, and the published stream of a scripted session with a private stretch holds none of
  its text) plus an overlay-half test that the captions kind ignores any unknown field (forward compatibility).

### D10. The native draw path: CPU painting per widget, shared text engine, uploaded only on change

```rust
// omuzaki-overlay/src/draw (#1036): not a contract; the shapes it relies on
pub struct Painter { /* font set, glyph cache, emote and image caches, scratch buffers */ }
impl Painter {
    /// Paints `view` with `style` and `palette` into `out`, premultiplied sRGB-encoded RGBA8 of `size` pixels.
    pub fn paint(&mut self, view: &View, style: &Style, palette: &OverlayPalette, size: UVec2, out: &mut Picture);
}
impl TextMeasure for PainterMeasure { /* the same shaping, without rasterising */ }
```

- **Where pixels are made:** each widget is painted **on the CPU** on the overlay thread into its own picture at its
  resolved size in output pixels, only when its **paint key** changes, and the render thread uploads that picture
  into the widget's dynamic texture with `paint_texture` (`PaintTarget::write_rgba` of the changed rectangle). The GPU
  then only composites (D11).
- **The paint key** is everything a picture depends on: `View::revision`; the resolved box size (a scene switch, a
  layout edit or a new program size); a revision of the widget's `Style`; the show palette's revision; the overlay
  language; and the revision of the painter's asset state (a fallback font, an emote image or a store image that
  arrived after a paint that had to draw `alt` or U+FFFD). Each is a `u64` counter or a size, so the key is compared,
  never hashed per frame, and a style, palette or language edit repaints every widget it touches while their logic
  keeps its state (D6's hot reload). A size change reallocates the picture once and recreates the texture only when
  the new size exceeds the texture's (textures are sized to the widget's largest box over the show's scenes).
- **Why the CPU, against reusing `omuzaki-face-api`'s vector `DrawList` and its rasterisers, or a new retained 2D list
  drawn on the GPU:** widgets change a few times a second, not every frame (fades and slides are composite-time,
  D5), so the cost is in the rare repaint, not the frame; a repaint of a chat box at 1080p is about a millisecond
  on a worker thread. CPU pictures are byte-exact across machines, so goldens are exact and the mock renderer composites
  the same bytes. The face rasterisers live in `omuzaki-face`, an implementation crate `omuzaki-overlay` may not depend
  on, and text as outlines through them would need a glyph path cache in the face crate. A GPU 2D list would put a text
  and shape contract into `omuzaki-render-api` that every renderer, the mock included, must implement; the composite
  step asks for one textured quad per widget instead.
- **Text** reuses what `omuzaki-stage`'s sheet already does: `harfrust` shaping (the workspace's), `ab_glyph`
  rasterising with sub-pixel x positions, `unicode-bidi` for visual order, and the Noto fallback chain pinned by URL,
  size and SHA-256 through the shared verified cache (nothing committed; tests use the subset fixtures). **Ask R2:**
  that code is lifted into a lib crate, `omuzaki-text` (`layer = "lib"`: shaping, the fallback catalogue, measuring and
  coverage rasterising, no registration), which the sheet and the painter both use. If the Render, stage and V0 lead
  prefers to keep the sheet as it is, #1036 creates `omuzaki-text` as a new crate without touching `omuzaki-stage`,
  and moving the sheet onto it becomes that lead's follow-up.
- **Fonts:** Inter for Latin (as the sheet, embedded, OFL), Noto Sans JP, KR, SC and TC for CJK, picked per run from the
  run's `lang` (the Han forms follow the language; text without one follows `OverlaySettings::language`), Noto Sans
  Arabic for right-to-left names, Noto Emoji for emoji (monochrome outlines in the run's ink, as the sheet). Colour
  emoji (Noto Color Emoji, COLRv1, OFL) are a follow-up (§6): they need COLR painting in the text lib. A character no
  font covers draws as U+FFFD in the body font and is counted, never as a blank.
- **Right to left:** each line is ordered with `unicode-bidi` (UAX #9) in the paragraph direction of its first strong
  character; `Align::Start` follows that direction. None of the six launch languages is right to left, but chat names
  are, and they must not scramble.
- **Emotes and images:** an emote span draws its image one line high, scaled with the line; its source is the **emote
  cache**, a service the BR-10 adapters fill (they hold the network grant and fetch emote images from the platform)
  and the painter reads by `EmoteRef`; a missing image draws `alt` as text and the picture repaints when it arrives.
  Animated emotes draw their first frame (animation is a follow-up: it would make the chat box repaint every frame).
  Images by `ContentHash` come from the project store (a sponsor logo), decoded once.
- **Shapes:** surfaces are rounded rectangles (`radius`), anti-aliased by exact coverage; the outline ring is a
  2-pixel dilation of the glyph coverage in the surface colour; bars and chips are rounded rectangles too. Coverage
  is applied to sRGB-encoded colour (as browsers and OBS do), so a native widget and the same design as a web widget
  look alike.
- **No allocation per frame once warm:** pictures, glyph caches (per font, size, glyph and quarter-pixel x offset;
  4,096 glyphs per show, least recently used out) and scratch buffers are reused; a resize reallocates once.
- **Padding:** the content box is the widget's box minus `0.5 × text size` on each side, so text never touches the
  rounded edge; `max_lines` and the box height bound the text, and text that doesn't fit is cut at a line with an
  ellipsis, never clipped mid-glyph (#1036's "no text is clipped by its own box").

### D11. The compositing step: one call after `Resolve`, in place, with a clean copy when needed (asks R1a, R1b)

**The ask to the Render, stage and V0 lead** (R1a, an `omuzaki-render-api` change of that area; this design doesn't edit
it):

```rust
// omuzaki-render-api
/// One screen-space layer: a textured quad blended over a finished frame.
pub struct Layer {                       // #[non_exhaustive]; Layer::new(texture, src, dst), with_opacity
    pub texture: TextureId,              // a premultiplied, sRGB, dynamic texture
    pub src: PixelRect,                  // the part of the texture to draw
    pub dst: LayerRect,                  // where, in the target's pixels; may extend past the target (clipped)
    pub opacity: f32,                    // 0..=1, multiplies the whole layer
}
pub struct LayerRect { pub x: i32, pub y: i32, pub width: u32, pub height: u32 }
pub struct CompositeReport { pub cpu: Seconds, pub gpu: Option<LateTiming>, pub layers: u32 }   // #[non_exhaustive]
pub struct LateTiming { pub frame: u64, pub time: Seconds }    // as GpuTimings: the newest finished composite
pub trait Renderer {
    // … as today, plus two methods with defaults that say Unsupported:
    /// Blends `layers`, in order, over the last frame rendered into `target`, in place.
    fn composite(&mut self, target: TargetId, layers: &[Layer]) -> Result<CompositeReport, RenderError>;
    /// Copies the last frame of `src` into `dst` (same size and alpha encoding).
    fn copy_target(&mut self, src: TargetId, dst: TargetId) -> Result<(), RenderError>;
}
// Capabilities gains `composite: bool`; limits: at most MAX_LAYERS = 64 layers per call.
```

- **The formula (exact, the reference):** on the target's stored 8-bit premultiplied, sRGB-encoded values (the
  encoding the program target, Spout, NDI and OBS use, render-api's `AlphaEncoding`), with `o = round(opacity × 255)`:
  `s' = (s × o + 127) / 255` for each channel and alpha of the layer texel, then `out = s' + (d × (255 − a') + 127) /
  255`. A straight-alpha target is premultiplied, composited and un-premultiplied with record D9's formulas. When
  `src` and `dst` are the same size (the normal case: widgets are painted at their output size) texels map 1:1 and the
  GPU result must match the mock byte for byte; otherwise sampling is bilinear on premultiplied values, within 1 level.
- **Why blend on the encoded values, not in linear light:** it is exactly what OBS does when it composites a browser
  source, and what any consumer of our premultiplied output does with it, so a widget looks the same drawn by us or
  by OBS. On the GPU this is a pass into an `Rgba8Unorm` view of the target.
- **Why a call after `render`, not a ninth `Pass` in the frame graph:** `Pass` is an exhaustive, fixed list that the
  scheduler, `PassTimes` and every renderer match; a new pass would be a breaking change. A separate call also lets the
  clean feed exist without a second render: the frame loop renders the program, copies it to a clean target only
  while some output takes `Feed::Clean`, then composites in place. **Why not in the stream layer:** live outputs share
  the program texture on the GPU (render-api, `read_target`'s note), so the composite must happen before they read it,
  and it has to run on every renderer the goldens and the mock use.
- **The mock** implements the reference formula and `conformance` gains checks for it: order of layers, opacity,
  clipping at the target's edge, `src` sub-rectangles, a straight-alpha target, `copy_target`, and that `composite`
  leaves pixels outside every layer untouched.
- **Ask R1b covers the frame loop** in `omuzaki-stage` (V0 integration is that lead's): after `render`, when a
  clean feed is active `copy_target(program, clean)`, then `OverlayFrame::prepare(renderer, size, live_scene,
  &mut layers)` and `composite(program, &layers)`, then the outputs. The `Vec<Layer>` is the loop's, reused. It
  also publishes the ramped A/V delay through an `AvDelay` (D9) that the App hands to the overlay host.
- **Cost:** the composite is at most 64 small quads; the clean copy, when on, is one full-target copy (about 16 MB of
  memory traffic at 1080p), which replaces record D9's second render for a clean feed (§6 notes the record update).

### D12. Which outputs carry overlays

| Output | Default feed | Clean option | Why |
|---|---|---|---|
| Spout, NDI | program (with overlays) | yes (`clean`) | viewers see what OBS gets; a person who builds overlays in OBS turns it off |
| Virtual camera | program | yes | the camera is shown to viewers as is |
| The clean window (for window capture) | clean | no | its purpose is the avatar alone |
| Recordings, the replay buffer | program | yes (`record.clean`, `replay.clean`, record D9) | a recording is what viewers saw |
| The editor viewport | program | a "Show overlays" toggle, on | the editor previews the stream |
| Offline renders, thumbnails, `render.preview`, goldens of the scene | clean | — | overlays are live state, not a picture of the scene |

The lead's steer holds: outputs carry what viewers see, and each has a clean option. `Feed` is `omuzaki-stream-api`'s
(`Program`, `Clean`), unchanged. A clean feed costs one copy per frame while it runs (D11), not a second render.

### D13. Web widgets: an OBS browser source and an editor preview, served locally, sandboxed

The lead's ruling: v1 ships **no embedded browser engine** in our output. Web widgets are served to OBS's browser source
and to a preview; native kinds draw in our output. The rest:

- **The server:** `omuzaki-overlay/src/web` runs a local HTTP server on `127.0.0.1` (default port 7480,
  `OverlaySettings::web_port`), on the `hyper` stack already in the workspace for the MCP port. Each web widget is
  served from **its own origin**, `http://w<token>.localhost:<port>/`, where the token is 128 random bits in 26
  lower-case base32 characters, kept in the machine-local approvals store and never in the document (a shared show
  must not carry working URLs). `*.localhost` resolves to loopback in Chromium, and so in OBS's browser source,
  without DNS. **Why an origin per widget:** browser storage (`localStorage`, IndexedDB, cookies, caches, storage
  events) is per origin, so two widgets open at once can never read each other's storage; paths under one shared
  origin would share all of it. The editor's "Copy URL for OBS" gives the URL; "New URL" rotates the token, and with
  it the origin, so nothing stored under the old one is reachable again. Requests whose `Host` isn't exactly a live
  widget's `w<token>.localhost:<port>` are refused (DNS rebinding, and no widget can name another's host), and the
  server never binds anything but `127.0.0.1`.
- **The feed:** Server-Sent Events at `/feed` on the widget's origin (one-way; a browser source needs nothing more, and SSE
  reconnects by itself). Messages: `hello` (the widget's settings, the palette, the language, the live scene), `event`
  (a `LiveEvent`'s kind, time and payload, only for kinds the manifest consumes, after `ChatFilter`), `scene` (the live
  scene changed), `visible`, `clear`, `alert` (`overlay.alert` aimed at this widget), and `time` once a second. The
  page script gets them through `omuzaki.js` (served by us), which also posts them with `postMessage` into the
  widget's frame in the preview.
- **The sandbox:** every response carries `Content-Security-Policy: default-src 'none'; script-src 'self'; style-src
  'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; media-src 'self'; connect-src 'self'` (plus each
  approved `network` host), `Permissions-Policy: camera=(), microphone=(), geolocation=(), display-capture=()`, and
  `Cross-Origin-Resource-Policy: same-origin`. So a widget reads only its own files and its own feed, with no network
  unless the person approved a host, no camera, no microphone and no file reads. Emote images and store assets are
  proxied under `/img/…` on the widget's origin so `img-src 'self'` holds. In the editor preview the page runs in an
  `<iframe sandbox="allow-scripts">` without `allow-same-origin` in addition.
- **Viewer data and storage:** OBS loads the page as a normal origin, so the CSP can't stop its script from writing
  to `localStorage`, IndexedDB, cookies or caches. The design doesn't pretend otherwise: a package whose `consumes`
  includes a kind carrying viewer data declares the `viewer_data` need. **Without the person's approval for this
  show, each event reaches the page only through a typed safe projection of its kind,** never a filtered payload:
  `register_safe_projection!` (in `omuzaki-overlay-api`'s `web` module) maps a kind to a function from its typed
  payload to a small struct holding only fields that identify nobody, and **a kind with no projection is withheld
  whole** (default deny, which covers plugin kinds and fields added later). The first-party table (in
  `omuzaki-overlay/src/web/projections/`, one file per kind): `follow` → `{platform}`; `sub` → `{platform, tier,
  months, gift}`; `sub_gift` → `{platform, tier, count}`; `cheer` → `{platform, bits}`; `tip` → `{platform,
  amount}`; `redeem` → `{platform, reward: {id, title, cost}}` (never `input`); `raid` → `{platform, viewers}`;
  `chat.message`, `chat.*` moderation kinds, `ai.line` and `voice.transcript` have none and are withheld. A test
  serialises every projection from a fixture holding a distinctive name, login, id, message and input, and asserts
  none of them appears. Each page load is served with `Clear-Site-Data: "storage", "cache",
  "cookies"` and `Cache-Control: no-store`, so nothing a page stored survives a reload of the source; an approved page
  can still keep data in its own origin while it stays open, which the approval card states, and no other widget
  can read it (its own origin, above).
- **Packages:** a web widget is a package: `widget.toml` (id `<publisher>/<name>`, version, title, summary, entry
  file, `consumes`, `needs`, a JSON Schema for its settings, licence and provenance like library items, ADR 0019) and
  its files. Installed packages live in the store by content hash and are **pinned by hash** in the widget's params
  (`package = "acme/hype-bar@1.2.0"`, `hash = "blake3:…"`); a changed file is a different package, inert until the
  person approves it again. Packages are untrusted code: the licence gate refuses a non-commercial licence in an
  exported show (as BR-21's sounds), and needs are approved per show on this machine (D5).
- **In our own output:** no. A web widget shows as an outlined box with its name in the editor viewport, and draws
  nothing into Spout, NDI or recordings; the Broadcast panel says "shown in OBS through its URL". **Why** (the
  question #1031 left for the Director, answered by the lead's ruling): an embedded engine (CEF, BSD-3-Clause) adds
  about 150 MB per OS, its own GPU process and update duty, and a second attack surface inside the render loop, for
  widgets that OBS already runs well; native kinds cover what must be in our output.
- **The editor preview:** "Preview" opens the widget's URL in the person's default browser, with a test event panel
  (fire a synthetic follow, chat line or transcript into that widget's feed). An in-editor preview through the OS's
  own web view (`wry`, MIT OR Apache-2.0: WebView2, WKWebView, WebKitGTK; no bundled engine) is a question for the
  lead (§7), since it is not "our own output".

### D14. Ops, scopes and agents

**Document ops** (`omuzaki-overlay`, edits, scope `edit:show`, `Undo::Auto`, one patch each; every edit checks the
resulting overlay with `check_overlay` before returning its patch; `show?` defaults to the current show, D2):

| Op | Does | Writes |
|---|---|---|
| `overlay.list {show?, scene?}` | query (`read`): widgets, kinds, effective layout for `scene` (default: the live one), document and live visibility, findings | |
| `overlay.kinds` | query (`read`): every widget kind with its summary, params schema, consumes and needs | |
| `overlay.add {show?, kind, id?, name?, layout?, style?, params?, copy_of?}` | a widget; `id` defaults to the kind (`chat_box`, then `chat_box-2`); `copy_of` copies a widget and its scene patches | `overlay/widgets/<id>`, maybe `overlay/scenes/*/<id>` |
| `overlay.set {show?, widget, name?, layout?, style?, params?}` | `layout`, `style` and `params` merge by top-level key (`null` removes a key), so an agent changes one field without restating the rest | that widget |
| `overlay.remove {show?, widget}` | the widget and every scene patch of it; the last one removes the `overlay` key's empty folders | `overlay/**` |
| `overlay.set_layout {show?, scene, widget, anchor?, offset?, size?, fit?, safe?, visible?, z?}` | the scene's patch; `null` clears a field back to the default | `overlay/scenes/<scene>/<widget>` |
| `overlay.reset_layout {show?, scene, widget?}` | removes one patch, or all of a scene's | |
| `overlay.copy_layout {show?, from_scene, to_scene}` | copies a scene's patches (after a scene duplicate) | |
| `overlay.configure {show?, safe?, palette?, language?, web_port?}` | the settings | `overlay/settings` |

**Live actions** (edit with an empty patch and `Undo::None`, #139; they call `OverlayControl`; scope `perform`):
`overlay.show {widget}`, `overlay.hide {widget}`, `overlay.toggle {widget}`, `overlay.clear {widget}`,
`overlay.alert {widget?, title?, text, kind?, duration_s?}` (empty `text` refused; at most 500 characters). An
unknown widget is an error naming the nearest id; an unknown kind lists the registered ones.

- **Why `edit:show`, not `edit:scene`** (#1034 proposed `edit:scene`): the overlay is a show key (D2), and automation
  adds `edit:show` (#951, in `Grants::agent_default` like the other edit scopes). Automation's D7 refuses `edit:show`
  from a show's own session, so a show can't rewrite its own overlays mid-stream, which is the safe default.
- **Why `perform` for the live actions:** they are the overlay's equivalent of triggering an expression on stream,
  and automation's base grants are `read` + `perform`, so a redeem can show an alert without an approval card. An
  agent's default grants lack `perform`, so **an agent that puts text in front of viewers needs a grant** for its
  session (the actor matrix of #1034 stands: `Grants::agent_default` is allowed the document ops and `Denied` the five
  live ops; with `perform` it is allowed).
- **What agents may call:** every op above with their own grants. Approving a widget's needs, rotating web URLs and
  installing packages are person-only UI actions with no op (automation D11's rule, for the same reason: an agent
  controls both sides of a request).
- **"No widget reads the camera or files without a scope," enforced in three places:** native kinds get no file or
  camera API at all (D5); `create` refuses a kind whose needs aren't granted for this show on this machine; the web
  server serves only the package's own files and its feed, under the CSP and the permissions policy (D13), and adds a
  host to `connect-src` only once approved.

### D15. Plugins (PL-03): two ways to add a widget

- **A web widget package** (D13) is the easy way: HTML, CSS and JavaScript, a manifest, any tool. It shows in OBS.
- **A native kind from a plugin** draws in our output. The plugin host (`omuzaki-plugin`, PL-01) adds `DynWidgetKind`
  objects to the catalogue as plugins load; ids are `plugin.<plugin>.<name>`. The WIT interface mirrors
  `WidgetKind::{info, check, create}` and `WidgetLogic::{on_event, on_command, step}`; `View` crosses the boundary as
  data (it is serde), so plugin logic never touches pixels and the same painter draws it. A plugin's `step` runs on the
  overlay thread under a fuel cap (0.25 ms per step); running out faults the widget (D6). The guest's `View` is
  decoded through a fallible path and validated like every view (D6), so a malformed one faults that widget only. Needs come from the plugin's
  manifest and are approved per show like first-party ones; a plugin kind gets the plugin's own grants intersected
  with the show's.
- A missing plugin leaves its widgets inert with a finding naming the plugin; the document keeps them.

### D16. Limits and budgets

| Limit | Value | When it trips |
|---|---|---|
| Widgets per show (`MAX_WIDGETS`) | 64 | `overlay.add` refused, naming the constant |
| Visible widgets per scene | 16 | refused by `overlay.add`, `.set`, `.set_layout`, `overlay.show` and `overlay.toggle` when it would show (document and live visibility); a hand-edited show over it is a finding, and the host draws the 16 highest `z` (ties: the lower id first), so foreground widgets such as alerts and captions stay |
| Scene patches per show | 64 × the show's scenes | (bounded by the two above) |
| Widget picture memory per show | 128 MiB: four copies of every widget's largest box over all scenes at the program size, × 4 bytes (three CPU slots of the triple buffer and the GPU texture), five for a `cross_fade` kind (the previous picture's texture), so the boxes themselves total at most 32 MiB (about 3.8 full 1080p screens) | `overlay.add`, `.set` (a larger `layout.size`) and `.set_layout` refuse an edit over it; a hand-edited show over it is a finding, and the largest widgets above the cap are inert |
| Painter caches per show | glyphs 16 MiB; decoded store images 32 MiB (emotes are the emote cache's, below) | least recently used out |
| Text: alert `text` and templates, chat `max_chars` | 500 characters | refused by the op or the params check |
| Images per widget; one image | 64; 4,096 × 4,096 and 16 MiB decoded | refused at `check`; a bigger image is scaled down once on load |
| Emote cache | 2,000 images or 64 MiB, least recently used out | evicted, repainted from `alt` until refetched |
| Glyph cache | 4,096 glyphs per show | least recently used out |
| Event queue per widget | 1,000 events (droppable kinds shed first) | dropped and counted in `WidgetLive` |
| Web widget packages | 32 MiB per package, 2,000 files | refused at install |

**Budgets** (each a criterion bench on `Dev`, the §10 rule; the first measurement may tune a number once, with the
lead's sign-off):

| Measure | Budget |
|---|---|
| `composite` of the four widgets of §4 (chat box 0.3 × 0.45, alerts 0.4 × 0.2, captions 0.6 × 0.15, AI bubble 0.35 × 0.2) at 1080p | ≤ 0.2 ms GPU p95 (the proposal of #1031, confirmed) |
| The same with a clean feed on (plus `copy_target`) | ≤ 0.35 ms GPU p95 |
| `OverlayFrame::prepare` on the render thread, no widget changed | ≤ 20 µs CPU, 0 allocations once warm |
| An upload when one widget changed (chat box at 1080p) | ≤ 0.1 ms CPU on the render thread |
| A repaint of the chat box with 8 lines and 2 emotes at 1080p (overlay thread) | ≤ 1.5 ms CPU median |
| `step` of all four widgets with nothing changed | ≤ 10 µs, 0 allocations |
| An event published to its pixels in the composited frame | ≤ 50 ms p95 at 60 fps (a frame to wake, a repaint, the next frame's upload) |

**The bench** (`crates/omuzaki-overlay/benches/overlay.rs`, #1036): the four widgets driven by a synthetic chat at 10
messages a second, a raid, and a caption stream, measuring the repaint per widget, the upload, the composite's GPU time
on the real renderer, and allocations per frame with a counting allocator (0 once warm).

### D17. Contract conventions (ADR 0054) and l10n

- **Paths, not re-exports:** `omuzaki-overlay-api`'s `lib.rs` declares `ids`, `layout`, `style`, `widget`, `check`,
  `paths`, `kind`, `logic`, `view`, `control`, `frame`, `chat`, `limits` and `l10n`, all `pub mod`, and re-exports
  nothing one by one; its `prelude` holds the ids only.
- **Generated module lists:** `omuzaki-overlay`'s `src/kinds/` and `src/ops/` are listed by `omuzaki_embed::modules`
  (`generated-mods` in its manifest), so a new kind or op is a new file and no task edits a shared list; this replaces
  #1033's stub files.
- **Non-exhaustive contracts, built through constructors:**

  | Type | Attribute | Built with |
  |---|---|---|
  | `WidgetDef`, `OverlaySettings`, `Layout`, `LayoutPatch`, `Style`, `PaletteSettings`, `WidgetKindInfo`, `Needs`, `KindCx`, `StepCx`, `MeasureStyle`, `View`, `AlertRequest`, `WidgetLive` | `#[non_exhaustive]` structs | `new(<required fields>)` plus `with_<field>`; `View::default()` is empty with `alpha` 0; `Needs::NONE` with `with_files`, …; `WidgetKindInfo::new(id, summary)` |
  | `ViewItem`, `Span`, `Content`, `Command`, `KindError`, `OverlayError`, `LiveState`, `PaletteSource`, `ImageRef` | `#[non_exhaustive]` enums | unit variants as they are; struct-like variants `#[non_exhaustive]` each, built with `ViewItem::text(spans)` (then `with_lang`, `with_align`, `with_size`, `with_max_lines`), `ViewItem::image(image, size)`, `ViewItem::bar(value, token)`, `ViewItem::chip(label, token)`; `Span::text(content, token)` and `Span::emote(emote, alt)`; `KindError::needs_grant(need)`; `OverlayError::unknown_widget(id, nearest)`, `unsupported(kind)`, `invalid(why)`; `LiveState::inert(why)`, `faulted(why)`; `PaletteSource::character(character, slots)` |
  | `Anchor`, `Fit`, `Align`, `TextSize`, `PaletteToken` | closed on purpose | the resolve arithmetic, the painter and the contrast walk match them exhaustively; a new value is a contract change by decision |

  A kind matches `Command` and `ViewItem` variants by name and answers a variant added later with
  `OverlayError::unsupported` (commands) or skips it with a counted warning (the painter, for items), never silently.
- **Registered keys:** the `overlay` show key (D2) through `register_show_key!`; nothing edits `ShowDef` or `SceneDef`.
- **l10n** (`docs/design/l10n-ops.md`): a kind's summary is `widget-<kind>` (`widget-chat_box`; a plugin kind's `.` becomes `-`, as for
  node ids), its params follow the schema walk (`widget-chat_box--max_messages`); messages
  use the prefixes `overlay-api-` and `overlay-`; the crate check learns the `widget-` registry in the contract task.
- **Tests across crates:** the overlay's whole path (feed, host, painter, mock composite) lives in an area test crate
  `omuzaki-test-overlay` only if a test needs more than `omuzaki-overlay` and the mocks; the draw goldens run in
  `omuzaki-overlay`'s own tests against `omuzaki-render-mock`.

## 4. Worked example: one show's widgets on disk

The default show `main` has two scenes, `main` (the avatar centre, a game behind) and `chatting` (the avatar large on
the left). The chat box moves right and grows in `chatting`; the alerts stay; the captions sit higher in `chatting`.

```toml
# shows/main/overlay/settings.toml
safe = [0.05, 0.05, 0.05, 0.05]          # left, top, right, bottom
language = "en"
web_port = 7480
[palette]
source = "character"
slots = { accent = "hair", surface_alt = "outfit" }
colours = { surface = "#101018e6" }
```

```toml
# shows/main/overlay/widgets/chat.toml
kind = "chat_box"
name = "Chat"
[layout]
anchor = "bottom_left"
offset = [0.0, 0.0]
size = [0.3, 0.45]
fit = "box"
safe = true
visible = true
z = 10
[style]
surface = "surface"
text = "text"
accent = "accent"
radius = 0.04
outline = false
text_size = 0.022
[params]
max_messages = 8
ttl_s = 30.0
max_chars = 200
hide_commands = true
delay_s = 2.0
```

```toml
# shows/main/overlay/widgets/alerts.toml
kind = "alerts"
name = "Alerts"
[layout]
anchor = "top"
offset = [0.0, 0.02]
size = [0.4, 0.2]
fit = "box"
safe = true
visible = true
z = 30
[style]
surface = "surface_alt"
text = "text"
accent = "accent"
radius = 0.1
outline = false
text_size = 0.03
[params]
duration_s = 5.0
gap_s = 0.5
dedupe_s = 10.0
priority = { raid = 5, sub_gift = 4, sub = 3, cheer = 3, tip = 3, redeem = 2, follow = 1 }
templates = { raid = "{from.name} brought {viewers} friends!" }
```

```toml
# shows/main/overlay/widgets/captions.toml
kind = "captions"
name = "Captions"
[layout]
anchor = "bottom"
offset = [0.0, 0.0]
size = [0.6, 0.15]
fit = "box"
safe = true
visible = true
z = 20
[style]
surface = "surface"
text = "text"
accent = "accent"
radius = 0.05
outline = true
text_size = 0.032
[params]
max_lines = 2
mode = "original"
follow_av_delay = true
```

```toml
# shows/main/overlay/widgets/bubble.toml  (the AI co-host's lines; the "AI" chip is always drawn)
kind = "ai_bubble"
name = "Co-host bubble"
[layout]
anchor = "right"
offset = [-0.05, -0.15]
size = [0.35, 0.2]
fit = "width"
safe = true
visible = true
z = 25
[style]
surface = "surface"
text = "text"
accent = "accent"
radius = 0.2
outline = false
text_size = 0.026
[params]
cps = 25.0
linger_s = 2.0
fade_s = 0.15
```

```toml
# shows/main/overlay/scenes/chatting/chat.toml   (only what this scene changes)
anchor = "right"
size = [0.32, 0.8]
```

```toml
# shows/main/overlay/scenes/chatting/captions.toml
offset = [0.0, -0.1]
```

In `chatting` at 1920 x 1080, `resolve` places the chat box at `Right` with `size [0.32, 0.8]` inside the safe
rectangle (1728 x 972 from (96, 54)): (1210, 108, 614, 864). The alerts and the bubble take their defaults. Switching
scenes keeps the chat lines, the alert queue and the captions: the same four instances run, only their rectangles
change (the captions re-break their lines once, for the new width).

## 5. Crates and contract changes

| Crate | Change | Owner |
|---|---|---|
| `omuzaki-overlay-api` (new, API) | D3 to D7, D17 | Broadcast and live |
| `omuzaki-overlay-mock` (new, mock) | `KindHarness`, `FakeControl`, `FakeMeasure`, fixtures | Broadcast and live |
| `omuzaki-overlay` (new, implementation) | the host, kinds, ops, painter, web server, the `overlay` show key | Broadcast and live |
| `omuzaki-speech` | `voice.transcript` (`Transcript`, `transcript_event`, feed-only) | Broadcast and live (audio) |
| `omuzaki-live-api` (**ask L1**, into #773 and #774 before they land) | `EventFeed` and its `Subscription`; `EventKindRegistration::feed_only()` (the engine ignores such kinds); `Source::Overlay { widget }` with `Source::overlay(widget)`; the normalised kinds `chat.delete`, `chat.clear_user`, `chat.clear` | Broadcast and live (the area's own contract) |
| `omuzaki-render-api`, `omuzaki-render-mock`, `omuzaki-render` (**ask R1a**, before A) | `Layer`, `LayerRect`, `CompositeReport`, `Renderer::composite`, `Renderer::copy_target`, `Capabilities::composite`, `MAX_LAYERS`; the mock's reference and conformance checks; the GPU implementation | Render, stage and V0 |
| `omuzaki-stage` (**ask R1b**, after A) | the frame loop: clean copy, `OverlayFrame::prepare`, `composite`, before the outputs; an `AvDelay` over its delay ramp (D9) | Render, stage and V0 |
| `omuzaki-text` (new, lib; **ask R2**) | the sheet's shaping, fallback fonts, bidi, measuring and coverage rasterising, shared | Render, stage and V0 (or Broadcast, D10) |
| `omuzaki-ops-api` | nothing new (`edit:show` is #951's) | |
| Spec | §9's `omuzaki-live` row names the overlay crates; BR-20's note says web widgets are browser sources in v1 | the Director (outside this change's fence) |

New dependencies, each checked against `docs/licensing/policy.md` in its task: `unicode-linebreak` (Apache-2.0,
#1187's); `ab_glyph`, `harfrust`, `unicode-bidi` (already in the workspace); `wry` only if the preview question is
answered yes. No embedded browser engine.

## 6. The task cut

The Director's rule since 2026-10-15 is large changes (`docs/maintainers/large-changes.md`), so the six covered
issues become four changes along the contract seams, plus the asks:

| Change | Covers | Crates | Depends on |
|---|---|---|---|
| **A. The overlay contract** | #1032 (re-cut: the layout lives in the show's `overlay` key, D2, so no `omuzaki-stage-api` edit), #1033 (re-cut: `StepCx`, `Content`, `Command`, `OverlayFrame`; no stub files, D17) | `omuzaki-overlay-api`, `omuzaki-overlay-mock` (new), each with its `[package.metadata.omuzaki]` table (the crate table in `crates/README.md` is generated from it) | #773, #952 (live-api), L1, R1a (`Layer`) |
| **B. The overlay host, kinds and ops** | #1034 (re-cut: `edit:show`; the extra layout ops), #1035, #1187 (both halves; re-cut: no `private` field, feed-only, `follow_av_delay`) | `omuzaki-overlay` (all but `src/draw`, `src/web`), `omuzaki-speech/src/events.rs`, the alert-sounds seed template, `omuzaki-cli` link | A, #139, #951, L1; captions' delay alignment needs R1b's `AvDelay` (0 with a finding until then) |
| **C. The draw path** | #1036 (fence: `omuzaki-overlay/src/draw/**`, its tests, goldens, the contact sheet, the bench) | `omuzaki-overlay/src/draw`, `omuzaki-text` if R2 is declined | A, R1a, R1b, R2 |
| **D. Web widgets** (new issue) | the server, the feed, the sandbox, packages and their licence gate, the browser preview | `omuzaki-overlay/src/web` | A |

**Asks to file** (one issue each, on acceptance): **R1a** (Render, stage and V0: `Layer`, `composite`, `copy_target`,
the mock and conformance, the GPU implementation; it lands **before A**, which names `Layer` in `OverlayFrame`),
**R1b** (the same lead: the frame loop's clean copy, `OverlayFrame::prepare` and `composite`, and the `AvDelay`; it
needs A's `OverlayFrame`, so it lands **after A**; D11),
**R2** (the same lead: `omuzaki-text`, D10), **L1** (folded into #773 and #774 by this area's lead, D7).

**What the design unlocks** (issues to file, each a change): the goal, timer, now-playing and sponsor-card kinds (one
change, `omuzaki-overlay/src/kinds/`); the editor panel (the Broadcast workspace's overlay list, layout handles on the
viewport, the style form from the schema, the approval card, "Copy URL for OBS"); colour emoji in the text lib;
animated emotes; layouts per output aspect (horizontal and vertical at once); plugin widget kinds (PL-03, phase 5);
and an update of record D9's sentence "a clean feed is a second target with its own render" to the clean copy of D11.

## 7. Open questions

1. **The ADR.** #1031's fence names a decision record; #2069's fence is this page only. *Recommendation:* the lead
   accepts the page; a short ADR (crates, the show key, the CPU draw path, the composite call, no embedded browser)
   follows as a Director-signed docs change, since decision records are the Director's.
2. **An in-editor web preview through the OS web view (`wry`).** It is not "our own output", costs no bundled engine,
   but adds a dependency and a platform surface to the UI. *Recommendation:* not in v1; the browser preview with the
   test panel is enough until people ask.
3. **R2's owner.** *Recommendation:* the Render, stage and V0 lead lifts the sheet's text into `omuzaki-text`, since
   it knows that code and the sheet must not change pixels; if that lead can't schedule it before change C, C builds
   the crate fresh and the sheet moves later.

## 8. Sources

Read on 2026-10-15: spec v3 §6, §7, §9, §10, §16, §25, §30, §31; issues #2069, #1031 to #1036, #1187 and the Director's
rules of 2026-10-15 on #2; `docs/design/{automation,render-api,record,audio-api,ai-performer,scenes,settings,l10n-ops}.md`,
`docs/design/ux/shell.md`; ADRs 0009, 0015, 0018, 0019, 0054; `crates/omuzaki-render-api/src/{pass,renderer,resource}.rs`,
`crates/omuzaki-stream-api/src/output.rs` (`Feed`), `crates/omuzaki-stage-api/src/scene.rs`,
`crates/omuzaki-ops-api/src/{scope,actor}.rs`, `crates/omuzaki-face-api/src/palette.rs`,
`crates/omuzaki-l10n-api/src/text.rs`, `crates/omuzaki-stage/src/sheet/layout/{text.rs,fallback/}`; WCAG 2.2 §1.4.3 and
§1.4.11; Unicode UAX #9 and #14; JLReq (kinsoku); the public docs of OBS's browser source (GPL-2.0, behaviour only),
StreamElements' and Streamlabs' widget models (behaviour only) and egui's layout (MIT OR Apache-2.0).
