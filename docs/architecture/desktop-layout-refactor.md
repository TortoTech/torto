# Desktop layout and UI refactor contract

This document is the migration contract for separating Torto's reading model,
layout engine, text backend, renderer, and desktop UI. It is intentionally
incremental: every phase must keep the reader usable and preserve source
mapping before the next dependency is removed.

## Ownership

- `publication` owns Reading IR, durable `SourceAnchor` / `SourceRange`, parsing,
  navigation targets, and fixed-layout text layers.
- `layout` owns style resolution, text-flow decisions, line breaking,
  pagination/regions, positioned frames, hit regions, and source maps.
- A text adapter owns font fallback, shaping, glyph/run construction, and final
  line realization. `LegacyParleyTextEngine` is the first adapter; a GPUI
  adapter can replace it without changing flow or source mapping.
- `renderer` compiles immutable layout frames into paint commands. It must not
  decide line breaks, pagination, selection semantics, or source mapping.
- `gpui-renderer` is the reusable final-paint adapter for GPUI. It converts
  neutral frame items and source-selection rectangles into GPUI elements, but
  owns neither document state nor layout decisions.
- `library` owns the UI-independent local shelf manifest, managed book and
  cover storage, import/open/remove transactions, and metadata updates. Desktop
  frontends consume this model instead of sharing either frontend's widget or
  window state.
- `sync` owns the UI-independent SQLite progress/annotation repository, vector
  clocks, tombstones, and durable `StoredHighlight`/note model. Frontends may
  present different controls, but they must not fork the persisted schema or
  annotation identity rules.
- The desktop app owns windows, commands, focus, input, menus, panels, and the
  final paint backend. GPUI becomes this owner only after the frame contract is
  covered by regression tests.

## Non-negotiable behavior

Each migration phase must preserve:

1. UTF-8 text hits and selections round-trip to the same durable source range.
2. Synthetic markers (for example list bullets) never enter authored ranges.
3. Translation text can map proportionally to the original source span.
4. Page and segment boundaries expose stable first/last source anchors.
5. Fixed-layout PDF/CBZ bypass reflow but join interaction and painting at the
   same frame boundary.
6. Re-layout after viewport, font, theme, translation, or OCR changes may issue
   new store-local line IDs but cannot invalidate persisted source locators.

## Target pipeline

```text
Reading IR + resolved style
        |
        v
TextEngine -- font fallback, shaping, glyph/run construction
        |
        v
PreparedLayoutBlock -- owned, measured prose/table/media/figure/quote
        |
        v
FlowItem stream -- Torto line-break/pagination decisions
        |
        v
Region/Page Builder
        |
        v
immutable LayoutFrame
  - positioned text lines / images / shapes
  - SourceMap
  - HitRegion
        |
        +--> renderer adapter --> Vello paint scene
        +--> gpui-renderer --> GPUI elements / final paint
        +--> reader interaction --> hit / select / navigate
```

## Migration order

1. Replace public Parley layouts with `TextLineId`, `TextLineSpan`, and
   `TextLayoutStore`; quarantine Parley in `text::legacy_parley`.
2. Introduce immutable `LayoutFrame`, move source maps and hit regions out of
   retained display lists, and make renderer consume frames only.
3. Split the monolithic layout crate into `style`, `text`, `flow`, `blocks`,
   `frame`, and `engine` modules while retaining the same public frame output.
4. Express classic page, continuous, focus, single-column, and double-column
   modes as `FlowItem` plus region/page builder policies.
5. Add `apps/gpui-probe` to validate windowing, input, text paint, IME, scaling,
   and frame replay; extract the local shelf model; then migrate desktop
   surfaces one by one through a production-shaped GPUI executable.
6. Remove egui/Vello/Parley only after GPUI owns every desktop surface and the
   source/pagination regression suite passes against the new text adapter.

## Dependency gates

- `renderer` and `reader` cannot have a normal dependency on Parley.
- `LayoutFrame` cannot contain egui, GPUI, Vello, Parley, EPUB, or PDF types.
- New renderer interaction APIs are prohibited; interaction belongs to the
  frame/source-map layer.
- A backend switch is rejected if it changes source ranges or page anchors
  without an explicit migration and fixture update.

## Implemented checkpoint

The first migration checkpoint is now represented in code:

- `text` owns the backend-neutral retained line, run, glyph, source-map, and
  interaction contracts. Parley is quarantined in `text::legacy_parley`.
- `style` resolves authored Reading IR against reader-controlled typesetting
  before any shaping or pagination takes place.
- `blocks` assigns backend-neutral breakability and flow roles to normalized
  blocks. Explicit page breaks already pass through this policy layer.
- `flow` owns measured lines, vertical glue, block breakability, page
  penalties, anchors, and stable half-open line fragments.
- `engine::RegionPlan` owns single/double-column geometry and the 800 DIP
  column cap independently of text shaping. `engine::RegionBuilder` now assigns
  paragraph lines, atomic media and safe table-row groups to regions through
  one penalty-aware half-open fragment contract.
- `frame` owns immutable `LayoutFrame`, cached page anchors, hit regions, and
  source/selection round trips. Reader and renderer consume this boundary.
- `TextLayoutStore::from_snapshots` is the validated adapter boundary. It
  assigns dense store-local line IDs and rejects invalid ranges or geometry;
  Parley's inline-box sentinel is normalized inside the legacy adapter.
- Production paragraph construction now enters `LegacyParleyTextEngine`
  through a backend-neutral `TextLayoutRequest`. Font family, weight, line
  height, styled spans, inline objects, indentation, alignment and the
  Greedy/Optimized policy no longer require `LayoutEngine` to construct a
  Parley layout directly.
- `linebreak::unicode` now owns UAX #14 opportunities as UTF-8 byte positions
  and the conservative phase gate for LTR/CJK/RTL text. `linebreak::measured`
  converts backend-neutral shaped cluster ranges/advances into Box/Glue and
  returns both backend counts and authored byte breakpoints. Parley and the GPUI
  probe now share that core-owned decision rather than treating either shaping
  backend's word/wrap boundaries as the source of truth. Knuth–Plass remains
  enabled only for the current ordinary space-delimited LTR subset; CJK, RTL,
  mixed scripts, hard-line content and inline boxes retain their tested greedy
  fallback.
- Production layout now has an explicit two-stage boundary. Reading IR is
  first resolved, resources are loaded, and text/tables/media are measured into
  owned `PreparedLayoutBlock`s; only then does the placement stage mutate page
  state. Image sizing is a pure region calculation rather than a dependency on
  `Paginator`.
- All prepared blocks are compiled into one ordered flow stream before
  placement. Prose contributes real measured lines, tables contribute safe row
  groups and penalties, images/separators and fitting figures contribute atomic blocks,
  authored line breaks contribute vertical glue, explicit page breaks
  contribute forced penalties, and source ranges contribute durable anchors.
  Unified typesetting now sends that complete stream through one
  `RegionBuilder` pass and emits the returned half-open fragments directly as
  text-line slices and table-row chunks. Inter-block glue collapses at physical
  region edges, so paragraph spacing is not stranded at the bottom of a page.
  Images, separators, figures that fit one region, and short quotes participate
  in the same pass as atomic blocks. Long quotes expand into measured text lines
  inside a backend-neutral `FlowScope`; the scope repeats continuation padding
  on every region and preserves one continuous quote decoration without leaving
  orphaned bars. Over-tall figures expand into ordered image blocks and caption
  lines, preserving authored caption-before/caption-after order and durable
  source mapping across pages. Only book-authored typesetting retains the
  compatibility emitter; unified typesetting has no block-class fallback gate.
- Reader-provided fonts use neutral `TextFontBlob` bytes. Reader and desktop
  APIs no longer expose Parley/Linebender `Blob` types; conversion is confined
  to the legacy adapter and the temporary font-catalog compatibility layer.
- Glyph runs carry `TextFontResource` (shared bytes, stable resource ID and
  collection index) rather than Linebender/Vello `FontData`; renderer-specific
  font handles are reconstructed only at the renderer boundary.
- `apps/gpui-probe` is an independent desktop executable. Its minimal
  `GpuiTextEngine` uses GPUI's real text system for ordinary LTR shaping while
  publishing only neutral snapshots to layout. `LayoutEngine` is generic over
  this boundary, and the probe now sends normalized Reading IR through the same
  production Style → Flow → Frame pipeline rather than manually constructing
  placements. It renders a title, prose, and an image from the resulting frame,
  supports scrolling and text hits, rebuilds on width changes, and restores
  selected geometry from `SourceRange`. GPUI text is first shaped without
  wrapping and converted to neutral measured clusters. Optimized LTR paragraphs
  use the same Knuth–Plass byte breakpoints as the Parley adapter; greedy LTR
  prose, pure CJK, and LTR+CJK mixed prose use Torto's UAX #14 greedy selector
  instead of GPUI wrap boundaries. RTL mixtures, hanging indents, inline
  objects, mixed font sizes, and footnote baselines take an explicit Legacy
  Parley fallback. Application-provided font bytes are registered with both
  GPUI and the fallback engine; only a failed GPUI registration closes the
  native gate. The probe retains one text/layout engine across width changes
  and loads the bundled Bitter font as a runtime native-path assertion. Debug
  probe runs compare coalesced authored text coverage and page anchors against
  the legacy engine even when the two engines choose different soft line
  breaks. `LineWidthProfile` resolves first/continuation line measures before
  entering greedy or Knuth–Plass selection, so ordinary first-line indents and
  hanging list indents no longer masquerade as content width. Both the Parley
  adapter and GPUI adapter now use that profile; the probe includes a hanging
  list item and asserts it stays on the native GPUI path.
- `crates/gpui-renderer` now owns both the reusable `GpuiTextEngine` shaping
  adapter and the `LayoutFrame` presentation boundary. The probe no longer
  contains either a private text backend or a second partial renderer: text
  native runs and fallback lines, inline images, rules, footnote markers,
  quotes, tables, raster/fixed-page replacement layers, separators, and source
  selection overlays all enter GPUI through `GpuiFramePresenter`. Its image
  cache remains backend-local and is explicitly cleared when a new frame
  generation is installed.
- `FrameInteractionMap` now exposes generation-local `FrameTextCursor` values
  at retained shaped-cluster boundaries plus a backend-neutral
  `selection_between` operation. It produces durable source ranges, copied
  text, and frame-coordinate rectangles without consulting a UI toolkit.
  Logical cursor movement crosses text regions, while selected source ranges
  resolve back to fresh cursors after re-layout. The GPUI probe consumes this
  contract for focused left/right movement, Shift+Left/Right expansion,
  Select All, and clipboard copy; resize restoration still persists only
  `SourceRange`, never generation-local region indexes or byte offsets.
- `rebook-reader` now treats immutable frames as its primary cache and exposes
  `ReaderFrameSpread` / `ReaderSectionFrame`. The Vello `PageDisplayList`
  compatibility cache is behind the default `legacy-renderer` feature, so a
  frontend can compile the complete reader navigation, source mapping,
  selection, fixed-layout, and prefetch suite with that feature disabled.
  `ReaderSession` stores its foreground shaper behind the neutral `TextEngine`
  contract and exposes application-engine constructors. Default constructors
  retain `LegacyParleyTextEngine` and background prefetch. UI-thread-bound
  engines deliberately disable the legacy prefetch worker, so a prefetched
  Parley frame cannot contaminate a GPUI-native session; uncached segments use
  the same injected engine synchronously until a backend factory/command worker
  can create an equivalent engine off the UI thread.
- `crates/library` owns the shared shelf/storage model. `apps/gpui-desktop`
  reads the real manifest and cover data, opens supported managed books into a
  renderer-independent `ReaderSession` with the window's shared
  `GpuiTextEngine`, presents its current `LayoutFrame`, turns pages, returns to
  the shelf, and repaginates on window resize. Pointer hits select semantic words
  through `ReaderSession`; only durable `SourceRange` values cross a resize, and
  the GPUI presenter regenerates selection geometry before cross-platform
  `secondary-c` copies the selected text. The reader boundary also exposes
  generation-local `ReaderTextCursor` values without leaking frame internals;
  the GPUI surface uses them for word-granular pointer drags, Shift+Left/Right
  expansion, current-spread Select All, and SourceRange-backed cursor recovery
  after repagination. Its native table-of-contents panel is
  populated from `ReaderSession::toc_items`, navigates through durable
  publication targets, derives the active row from each reader snapshot, and
  reserves viewport width before repagination. Real-window checks cover opening
  and hiding the panel, jumping to a later chapter, preserving the active row,
  and repaginating again after maximization. It no longer launches the legacy
  desktop process or constructs its foreground frames with the default reader
  text engine. The old desktop still uses the default compatibility feature and
  therefore keeps its existing display-list and asynchronous prefetch behavior
  while migration continues.
- `apps/gpui-desktop` and the legacy desktop now consume the same
  `rebook-sync` store. GPUI loads persisted highlight ranges into separately
  styled source overlays, can create a highlight from the active durable
  selection, and observes it again after closing and reopening the book. The
  real-window persistence check uses `REBOOK_SYNC_DATABASE` to isolate its
  SQLite file from production annotations. It also loads and saves the same
  versioned `LocatorV1` progress record used by the legacy desktop. Injected
  application text engines can open directly at a locator without compiling an
  unused first section; page turns, TOC jumps, and reader shutdown persist the
  current source-backed locator, while stale locators fall back to the beginning
  without preventing the book from opening. A real-window check turns a real
  EPUB into its second section, returns to the shelf, reopens it, and observes
  both the restored status and second-section frame. GPUI now also owns a native
  single-line note input with IME, UTF-16/UTF-8 selection conversion, clipboard
  editing, and contextual keyboard bindings. The annotation sidebar reads the
  shared store, navigates through durable `SourceRange` anchors, and creates,
  updates, or tombstones notes through the same repository as the legacy
  desktop. Reusing an exact-range highlight for a note avoids duplicate source
  overlays. A real-window check covers Chinese input, input-local Select All,
  Enter-to-save, sidebar reflow, and note persistence after returning to the
  shelf and reopening the book. `ReadingMode` and
  `ReaderPresentationPolicy` now live beside the reader session rather than in
  the egui preferences module. Both desktop frontends resolve the requested
  mode through that toolkit-neutral policy; it owns focus-to-scroll mapping,
  unsupported-focus fallback, footnote-marker layout flags, and the legacy
  book-style paragraph-gap profile while leaving animation and sidebar state in
  the frontend. `ReaderScrollLayout` now composes immutable
  `ReaderSectionFrame`s into the continuous/focus vertical surface. Frame crop,
  retained-content bounds, restored inter-page leading gaps, content-to-frame
  coordinate conversion, source-range positioning, and visible-frame selection
  therefore no longer depend on Vello display lists. The egui adapter retains
  only legacy page painting and cross-page quote-accent bridging while migration
  continues. The production-shaped GPUI desktop now consumes the same
  `ReaderScrollLayout` directly: it paints every immutable crop in the current
  reading unit as one continuous surface, routes pointer and keyboard selection
  through its neutral text-interaction API, restores exact source anchors after
  open, navigation, and resize, and synchronizes the persisted visible position
  as the tracked viewport crosses frame boundaries. Resume now retains the
  persisted anchor itself while resolving the new viewport, rather than using
  the beginning of the newly paginated frame as a replacement scroll target.
- Focus reading now has a toolkit-neutral identity and viewport contract in
  `rebook-reader`. `ReaderFocusUnit` retains durable source and paint ranges;
  `ReaderFocusGeometry` maps those ranges into the stitched scroll coordinate
  space; and `ReaderFocusViewportPolicy` owns short-unit centering, tall-unit
  top/bottom bounds, nearest-unit snapping, and target offsets. Its explicit
  `ReaderFocusViewportTarget` maps one content coordinate to one viewport
  coordinate, so a frontend without the legacy synthetic half-viewport padding
  can center a short unit or top-align a tall unit without reinterpreting an
  offset. The legacy offset method remains stable for the egui surface.
  Half-open anchor containment, initial-unit resolution, and previous/next
  boundary outcomes are shared as well. `compile_focus_candidates` now lowers normalized Reading IR
  before geometry: it attaches leading headings to the first readable unit,
  groups nested list descendants, filters footnote definitions and fixed-page
  image layers, preserves quote/table paint ranges, and classifies text, table,
  image, rectangular, and structured activation semantics. The legacy desktop
  keeps only presentation payloads such as localized image labels, clipboard
  table text, resolved footnote text, animation clocks, and egui input state.
  This prevents focus navigation from rebuilding source geometry from Vello or
  egui rectangles and gives a future GPUI focus surface the same durable unit
  contract. `ReaderFocusState` is the accompanying toolkit-neutral reducer: it
  owns the active unit index, durable regeneration anchor, and the mutually
  exclusive action/footnote affordance. Select and move commands emit explicit
  unchanged, empty, selected, or directional-boundary transitions; the frontend
  attaches scroll animation, persistence, and cross-section navigation only
  after reducing that command. `ReaderSession::current_focus_layout` now joins
  prepared Reading IR candidates to `ReaderScrollLayout` geometry once for every
  frontend, including shared table spacing, heading-anchor restoration, inline
  footnotes, and resolved linked footnote definitions. The GPUI desktop consumes
  that model and reducer directly: Up/Down selects and
  centers semantic units, directional boundaries enter the adjacent reading
  unit, the active unit is painted from its durable source ranges, and progress
  saves the active unit range rather than a generation-local frame position. Alt
  presents resolved footnotes in a native GPUI popover; Tab presents independent
  chat, highlight, and annotation actions. Highlight and annotation reuse the
  shared source selection and repository instead of introducing GPUI-only data.
  Focus chat is backed by the frontend-neutral `rebook-assistant` crate. Its
  `ChatSession` reducer captures an immutable request snapshot, keeps the active
  paragraph as hidden `SourceRange`-backed model context, and rejects stale
  streaming/completion events by session and request identity. GPUI owns the
  floating conversation presentation and input focus, while provider loading,
  request construction, and OpenAI-compatible HTTP execution run through the
  shared runtime on GPUI's background executor. The legacy desktop already
  reuses the shared role, turn, selection, and reading-context types. Normalized
  Reading-IR search and the stable citation URI/marker protocol now live in the
  shared crate. `ReaderSession::book_source` exposes the lazy source boundary
  without exposing a parser or layout backend, and GPUI focus chat installs the
  read-only `BookSearchToolHost` on its background request. The legacy desktop
  consumes the same search and citation functions while retaining its mature
  rewrite and annotation implementations. Their OpenAI-compatible tool-call
  loop now runs through the shared
  `OpenAiToolLoop`: it repairs structured arguments at the provider boundary,
  correlates every result by call identity, preserves transcript ordering, and
  enforces the model/tool round limit without depending on either frontend.
  The shared runtime owns both async and background-blocking capability-backed
  completion entry points. Assistant-produced `BlockRewrite`, translation input
  and output DTOs, plus generic annotation create/update/delete actions now live
  beside that runtime rather than in the egui application. Annotation
  confirmation is coordinated transactionally through
  `AssistantAnnotationTarget`: every successful application produces an opaque
  undo value, and a later failure rolls earlier actions back in reverse order.
  `PendingAnnotationActions` owns the toolkit-neutral unresolved batch: failed
  confirmation retains it for retry, while successful confirmation or explicit
  cancellation clears it. `rebook-session::StoredHighlightMutationTarget` joins
  the UI-independent highlight store to this protocol, and
  `DocumentAssistantToolHost` exposes source-backed search, selection and
  annotation tools against an isolated working view. GPUI focus chat now
  receives the resulting batch without writing to storage, blocks further
  requests until it is resolved, and presents native confirm/cancel controls.
  Both frontends therefore use the same atomic coordinator, reload durable state
  after success or rollback, and retain an exact `SourceRange` through the
  shared boundary and GPUI EPUB integration fixture.
  `RewriteBookSource` and its opaque
  undo transaction have also moved into the shared crate; the legacy module is
  now only a compatibility re-export. Translation mode and provider DTOs are
  shared as the seam for the larger translation source.
  `ParagraphStructureSource` now lives in `rebook-reader`; its sentence,
  formula, paired-punctuation, linked-footnote, inline-footnote, and bilingual
  companion handling moved with it, while the desktop plugin became a
  compatibility re-export. Both frontends can therefore layer the exact same
  source-preserving sentence presentation. `TranslationBookSource` has likewise
  moved into `rebook-assistant` with its complete fixed-page grouping,
  table/quote/figure, inline style, baseline, footnote-link, replace/bilingual,
  visibility filtering, and source-overlap test suite. The former desktop
  translation module is now only a compatibility re-export.
  `rebook-session::DocumentSourcePipeline` now owns the one canonical source
  composition order used by both desktop frontends: parser/PDF OCR source,
  transactional rewrite, translation, then sentence-structure presentation.
  It retains typed handles for mutation without exposing a toolkit or layout
  backend. Inactive layers are tested to preserve the canonical Reading IR and
  exact `SourceRange`; combined tests prove rewrite/translation/structure order
  and fixed-page translation policy. GPUI now opens `ReaderSession` and book
  search against that same outer source, with an end-to-end EPUB assertion that
  the session consumes the pipeline identity. GPUI pending assistant-mutation
  presentation and asynchronous feedback now use the same explicit resolution
  contract as the legacy UI; durable GPUI highlight/annotation writes use the
  shared transaction target.
  Open and resize still
  restore the exact persisted SourceAnchor before any user-initiated centering,
  preserving resize and resume stability.
- `rebook-session::ReaderDocumentPreferences` now owns the toolkit-neutral
  document typography, typesetting, spread, reading-mode, and selection
  settings. Its compatibility loader reads the existing version-1
  `reader-settings.json` while ignoring legacy egui-only interface, theme, and
  shortcut fields; normalization and legacy spacing migration therefore happen
  once. Both desktop frontends resolve the same `ReaderStyle` and
  `ReaderPresentationPolicy` through this contract. The GPUI desktop no longer
  hardcodes unified typography or presentation defaults, and its pointer/focus
  selection behavior uses the resolved shared granularity and mode. Typed
  `ReaderDocumentPreferenceChange` commands now normalize edits before they can
  enter layout cache keys. The shared writer atomically merges those fields into
  the existing version-1 file, preserves frontend-owned and unknown sibling
  fields, and refuses to overwrite an unknown version. The legacy settings
  controller uses this same command/normalization path while retaining ownership
  of interface theme, language, shortcuts, providers, and sync credentials. The
  GPUI desktop now exposes those document settings through a native draft-based
  settings overlay. Save applies the shared commands, rebuilds from the exact
  durable `SourceAnchor`, restores selected `SourceRange` values against the new
  frame, and only then atomically commits the compatible file; cancellation does
  not reflow or persist. Fixed-layout publications keep the shared classic-mode
  fallback explicit, and the modal blocks background reader interaction while it
  is open.
- `rebook-session::PdfDocumentMetadataCommand` is now the toolkit-neutral
  mutation boundary for generated PDF title/author metadata, generated TOC
  drafts, and OCR special-page roles. It normalizes provider output, physical
  page order, TOC depth, confidence, and duplicate page-role assignments before
  a persistence target can observe them. The legacy desktop implements the
  target with its existing version-1 files and sync dirty markers; AI extraction,
  reviewed-TOC save, and OCR page-role save no longer write those stores directly.
  The compatibility modules only own the old byte format and generated-TOC
  source wrapper while GPUI can consume the same command protocol later.
- `rebook-session::PdfOcrSourceController` now owns the thread-safe choice
  between a canonical fixed-page PDF source and its OCR reflow source. The
  shared boundary exposes the stable `PdfOcrViewMode`, physical-page anchor
  prefix, original/reflow URL mapping, and a pure toggle transition plan. View
  mode persistence runs through `PdfOcrViewModeMutationTarget` before the source
  changes; a failed write leaves the controller untouched, while an explicit
  post-layout rollback restores the in-memory source even if repairing the
  preference file also fails. Reader and egui presentation code import these
  session types directly rather than obtaining document state from the plugin
  module. `PdfOcrReflowBookSource` now also owns the completed-page-to-Reading-IR
  boundary: reflow section grouping, physical-page anchors, generated spine and
  TOC remapping, original-image special pages, and lazy OCR resources are shared
  and toolkit-neutral. A small `PdfOcrMarkupEngine` adapter keeps provider-owned
  Markdown parsing and sanitization outside the session crate. The legacy plugin
  now decodes version-1 files, repairs cross-page Markdown, resolves compatibility
  resource paths, and performs provider requests before handing a neutral OCR
  document to the shared source.

The probe is deliberately not the production desktop and does not replace the
existing egui/Vello path. GPUI 0.2.2 exposes shaped lines and opaque font IDs but
not system-font bytes. The adapter therefore snapshots backend-neutral line and
cluster geometry for layout, hit testing, and selection, emits a neutral
`TextNativeRun` paint descriptor, and drops the GPUI shaping objects before the
frame is published. The probe currently reshapes those authored slices through
ordinary GPUI text elements at paint time. A future production GPUI renderer
may add an external shaping cache, but that cache must not become part of
`LayoutFrame`. The existing renderer remains the typography reference until all
unsupported text cases have parity fixtures.

## Next migration gates

1. Expand the shared `GpuiTextEngine` beyond its phase-gated LTR/CJK subset.
   Inline objects remain on the legacy adapter because they currently occupy a
   zero-length byte boundary rather than a display-text placeholder; native
   support must preserve their advance, line assignment, and SourceRange before
   lifting that gate. The first source-coverage and page-anchor parity gate is
   active; add exact pagination point fixtures as both engines gain equivalent
   font/style inputs before enabling it outside the probe.
2. Add language-aware hyphenation, CJK spacing and punctuation rules, and BiDi
   paragraph handling behind the neutral text boundary. Unsupported cases must
   continue to select an explicit tested fallback rather than leaking backend
   line-breaking decisions into layout.
3. Add GPUI IME, accessibility, scale-factor, and broader mixed-script
   fixtures. The probe now covers pointer hit-testing, logical cluster-based
   keyboard selection, clipboard copy, scroll, resize reflow, and source
   restoration; visual `BiDi` caret movement remains gated on a future text
   engine capability.
4. Move the remaining desktop document-session concerns out of the legacy UI:
   rewrite/translation/structure sources, PDF OCR/TOC metadata, remote sync, and
   asynchronous open/prefetch command delivery. Local durable
   locators, annotation CRUD, native TOC navigation, resolved focus footnotes,
   the focus highlight/annotation actions, frontend-neutral chat state, provider
   execution, ordinary source-backed focus questions, repaired JSON handling,
   the provider tool-call state machine, normalized book search, and citation
   protocol are now shared. GPUI consumes the read-only search host. Rewrite and
   translation command DTOs plus atomic confirmed-annotation semantics are also
   shared; the legacy persistence adapter proves rollback and source-range
   preservation. The transactional rewrite, translation, and sentence-based
   structure derived sources are shared as well. Both apps now compose those
   sources through `DocumentSourcePipeline`, including the fixed-page policy,
   and GPUI opens its reader and mutable assistant host against the resulting
   presentation source. GPUI highlight and annotation CRUD, including staged AI
   batches, use the shared atomic confirmation adapter with reverse rollback,
   exact source ranges, and explicit confirm/cancel presentation. Shared
   document preferences now load, mutate, normalize, resolve, and atomically save
   through `rebook-session`; the native GPUI settings surface now consumes those
   commands and preserves source position and selection across reflow. Generated
   PDF bibliographic metadata, TOC drafts, and OCR page roles now mutate through
   a shared session command/target contract. The original/OCR source switch,
   physical-page mapping, transition planning, and failure-safe mode mutation
   are shared as well. Completed OCR pages/resources now become reflowable
   Reading IR through the shared source, including stable physical-page and TOC
   anchors. Next move the recognition-job lifecycle and its compatibility
   persistence/sync adapters out of the legacy UI, complete remaining
   frontend-owned settings parity, and add TOC tree
   expansion and active-row auto-scroll. Reading-IR
   candidate compilation, unit identity, stitched geometry, centering, snapping,
   navigation boundaries, active source state, and the command/state reducer are
   now consumed by both desktop paths; animation clocks, popup composition, and
   raw toolkit input remain frontend concerns. Both desktop frontends already
   consume `ReaderScrollLayout` for continuous reading.
   Pointer drag and keyboard selection expansion already use generation-local
   reader cursors while persistence remains based only on source ranges.
5. Replace synchronous cache misses in application-engine sessions with a
   command/factory based worker that can create a backend-equivalent text engine
   on its owning thread. Until full style/font parity passes, keep the existing
   legacy text adapter only as an explicit per-request fallback inside
   `GpuiTextEngine`.
6. Do not remove egui, Vello, or Parley until all normal-format and fixed-layout
   regression suites pass through the new backend.
