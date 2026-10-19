# Rift native UI specification

**Status:** v1 (2026-10-18, `screens-chrome`). This is the implementation spec for
the hand-written immediate-mode UI in `crates/rift-desktop/`. It is distilled from
the legacy webview UI — its stylesheet (all numbers), app shell (state machine /
layout composition) and component tree (every screen) — and cross-checked
pixel-by-pixel against the acceptance screenshot `docs/vulkan-migration/reference-old-ui.png` (1160x740).

**Audience:** every screen implementer (`screens-chrome`, `screens-pages`), plus
the Lead for `theme.rs`/`state.rs` integration. Where this file and the old CSS
disagree, the *measured screenshot* wins for the 1160x740 reference frame; the
CSS is normative for every other window size / responsive rule.

**Provenance note:** the legacy webview frontend (`ui/`) is no longer part of the
tree. Where this document cites `ui/styles.css`, `ui/src/**` or `ui/logo.svg`, it
is naming that legacy source, kept as the provenance for the measured numbers
(available in git history); the files themselves no longer exist. The archived
reference screenshot is `docs/vulkan-migration/reference-old-ui.png`, and the
logo source/texture live in `crates/rift-desktop/assets/logo/`.

Measured numbers below were taken with a Python/Pillow column-row scanner over
`docs/vulkan-migration/reference-old-ui.png`. The screenshot was captured in a WebView with ClearType
subpixel antialiasing, so individual glyph edge pixels have red/blue fringes;
measurements therefore use the *ink bounding box* (first/last non-background
row/column), which is ±1 px. The native renderer draws grayscale-antialiased
glyphs, so exact glyph coverage will differ; **layout boxes and color fills are
the acceptance target, not literal glyph pixels.**

---

## 0. How to read this file

* **§1–§5** are global: geometry, tokens, typography, icons, textures.
* **§6** is the app chrome (titlebar, sidebar, player bar, queue, overlays,
  scrollbars, menus, focus) that surrounds every view.
* **§7** is the views, one section each, in the old app's `View` order.
* **§8** is the interactions/states master table.
* **§9** is the `RIFT_PREVIEW=1` data (needed for screenshot parity).
* **§10** maps the legacy app state machine onto the native state model.
* **§11** lists the `Action` vocabulary screens emit (proposal for Lead).
* **§12** ASCII layout diagrams with pixel measurements.
* **§13** region-by-region description of `docs/vulkan-migration/reference-old-ui.png` (the only screen
  the reference shows; it is Home + expanded queue + hovered recent row).
* **§14** acceptance checklist.
* **§15** known discrepancies / requests for other owners.

The old UI's text was rendered with `font-family: "Inter", "Segoe UI", ...` and
**no `@font-face`**, so on the Windows capture machine it fell back to **Segoe
UI**. That is why line heights in the screenshot are `1.33 × font-size` (Segoe
UI's `(hhea ascent + descent) / upm = (2210 + 514) / 2048`). The native app
embeds Inter (`crates/rift-desktop/assets/fonts/`) for a redistributable font; **render
positioning must use the line-height table in §4.2, not Inter's natural
1.21 ratio**, or every row will be ~10 % short against the screenshot.

---

## 1. Window geometry

Window: **1160 × 740 logical px** (legacy window config: 1160x740, min 940x620,
`decorations: false`, resizable, centered). `scale_factor = 1.0` for the
reference shot.

The three horizontal bands, measured from `docs/vulkan-migration/reference-old-ui.png`:

| Band | y (incl) | height | color |
|---|---|---|---|
| Titlebar | 0..45 | **46** (45 content + 1 px bottom border at y=45) | `#111113`, border `#232328` |
| Body | 46..655 | **610** = sidebar/content row | sidebar/titlebar `#111113`, main `#0e0e10`, queue `#121215` |
| Player bar | 656..739 | **84** (1 px top border y=656 + 83 content) | `#111113`, border `#232328` |

Body row composition (left → right):

* **Sidebar** `x = 0..334` → **width 335 px** in the reference capture, including
  its 1 px right border drawn at `x=334` (CSS default is **216 px**, min 160,
  max 420 — see §6.2 and §15; the screenshot was taken after the author drag-
  resized the sidebar, so `--screenshot --preview` must set 335 to reproduce it).
* **Content column** `x = 335..1159` (width 825), split vertically into:
  * **main** scroll area: `y = 46..420` (375 px tall), `#0e0e10`,
    `padding: 20px 24px`, and `scrollbar-gutter: stable` → a **10 px scrollbar
    gutter is always reserved on the right** (x = 1150..1159) whether or not the
    view overflows. content box: `x = 359..1125` (767 wide) at sidebar 335;
    `x = main.x + 24 .. main.right() - 10 - 24`.
  * **queue panel**: `y = 421..655` (235 px incl 1 px top border at y=421) when
    it has tracks and is expanded. Collapsed = 45 px (1 + 44 header). No queue
    panel at all when the queue is empty or the view is Settings.
* **Player bar** pinned to the window bottom, full width.

Overlays are window-anchored, not band-anchored: toast `bottom: 110px; left:
18px`, update banner `bottom: 110px; right: 18px`, popup menus at their anchor.

Root paint order (painter's algorithm; each is a separate layer when it needs a
clip):

1. clear to `#0e0e10` (theme.bg)
2. titlebar band + border
3. body: sidebar (+ its resize handle), main (clipped to main rect), queue panel
4. popup menus (above everything in the band that opened them; draw after all
   app chrome)
5. player bar + border
6. toast, update banner
7. (no window shadow / no rounding — `decorations:false, shadow:false`)

### 1.1 Responsive rules

* Cards (§7.1.2): `repeat(auto-fill, minmax(160px, 1fr)); gap: 18px`. In code:
  `n = max(1, floor((content_w + 18) / (160 + 18)))`, `card_w = (content_w -
  (n-1)*18) / n`, `cover = card_w - 20`.
  Reference (content 767): n = 4, card_w = 178.25, cover 158.25 (measured integer spans 158/159 px).
  Default 216 sidebar at 1160 (content 886): n = 5, card_w = 162.8, cover 142.8.
* Track row columns use flex: 44 px fixed tail (`dl` 28 + `dur` 40 + actions 56)
  and two equal flex:1 columns (`meta`, `spacer`), so the album column stays
  centred as the window resizes; see §7.1.3 formula.
* Window minimum 940x620 must keep the layout intact (grid drops to 4 or fewer
  columns; titlebar search box becomes `min(480, 42vw)` = ~395 px at 940).

---

## 2. Design tokens (source of truth: `ui/styles.css` `:root`, mirrored in
`crates/rift-desktop/src/theme.rs`)

### 2.1 Colors

| Token | Hex | RGB | Used for |
|---|---|---|---|
| bg | `#0e0e10` | 14,14,16 | window clear, main scroll background |
| bg-raised | `#151518` | 21,21,24 | searchbox bg, cards' hover, track-row hover/playing, menus, settings rows, secondary buttons, `skeleton` tiles |
| bg-hover | `#1d1d21` | 29,29,33 | sidebar active/hover row, icon-button hover, `.tab` hover? (tabs keep raised; hover only changes text), switch off |
| border | `#232328` | 35,35,40 | every 1 px divider/border: titlebar, sidebar, queue, player, searchbox, card kebab button borders? (no), settings rows, menu border, tabs border |
| text | `#ececef` | 236,236,239 | primary text, play button fill |
| text-dim | `#8b8b93` | 139,139,147 | secondary text, icons at rest, placeholders, counts, 10 px section labels |
| accent | `#8b3df0` | 139,61,240 | brand: search button, tabs active, play-all pill, sidebar active icon, hearts (liked), seek/volume played fill, sliders, logo, active repeat/shuffle icon, switch-on, danger text in menus (quirk: `.menu-item.danger` uses accent, not danger) |
| accent-dim | `#6d28d9` | 109,40,217 | accent hover/pressed (search button hover, play-all hover, primary button hover) |
| danger | `#e5484d` | 229,72,77 | failed-download row icon (`.trow-dl .ibtn.error`), error affordances |
| focus | `#c4a0ff` | 196,160,255 | keyboard focus ring (2 px outline, offset 2; inset -2 on rows/cards) |
| — titlebar/sidebar/player bg | `#111113` | 17,17,19 | the three bars |
| — queue panel bg | `#121215` | 18,18,21 | queue panel surface |
| — toast | bg `#2a1216`, border `#b81e30`, text `#ffb3bb` | | error toast |
| — update banner | bg `#1c1c20`, border `#232328`, text `#ececef` | | launch-time update notice |
| — scrollbar thumb | `#2a2a30` | 42,42,48 | 10 px scrollbars, radius 5; thumb hover `#3a3a42`; track transparent |
| — cover fallback gradient | `#1c1c22` → `#141417` | | 135° linear, from top-left to bottom-right |
| — menu shadow | `rgba(0,0,0,0.55)`, 0 10px 30px | | menus |
| — update banner shadow | `rgba(0,0,0,0.4)`, 0 8px 24px | | banner |
| — searchbox focus | border + 1 px ring `#c4a0ff` | | `:focus-within` |
| — drag-row (sidebar/queue reorder) | bg `#151518`, shadow `0 4px 14px rgba(0,0,0,.45)`, opacity .92 | | while dragging |
| — sidebar resize handle hover | `rgba(139,61,240,0.35)` 5 px | | |
| — ytdlp found | text `#4ade80`, bg `rgba(74,222,128,.12)` | | |
| — ytdlp missing | text `#f87171`, bg `rgba(248,113,113,.12)` | | |
| — window close hover | bg `#e81123`, glyph `#fff` | | |

### 2.2 Radii

| Radius | px | Used for |
|---|---|---|
| radius_sm | **5** | `.ibtn` icon buttons, `.np-cover` |
| radius_sm2 | **4** | `.trow-cover`, `.qrow-cover` (40/34 px covers) |
| radius_md | **6** | searchbox, sidebar rows, tabs? (999), track rows, menu items, cover tiles `.card-cover`, `.rename-input`, `.side-new-input`, `.ytdlp-input`, playlist `.menu...`, focus ring default |
| radius_lg | **8** | `.card` (hover bg), menus, album-art, toast, settings rows, `.btn-secondary`/`.btn-primary` |
| radius_xl | **10** | update banner, settings rows (also 10) |
| radius_pill | **999** | play-all / head-btn pills, tabs, switches, ytdlp badges, artist avatar circle |
| cover round | 50% | artist card cover + artist page avatar |

### 2.3 Spacing scale (`ui/styles.css` uses raw px; these names are for code)

`4, 6, 8, 10, 12, 14, 16, 18, 20, 22, 24, 26` px. The important ones:
2 (icon inset/gaps), 4 (menu padding), 6 (menu item radius / tab radius),
8 (menu item pad-y, icon-button size, section label margin), 9 (logo gap, card
name top margin), 10 (sidebar/player gap, card padding), 12 (side border pad,
row gaps), 14 (h2 bottom margin, seek row x padding), 16 (queue head pad-x,
player pad-x), 18 (card gap, h2 top margin, main padding-y = 20, sidebar/window
pads), 20 (main padding-y), 22 (listen: logo right pad, play-all x pad), 24
(main padding-x, card grid bottom), 26 (card grid bottom = 26).

### 2.4 Component-state colors (derived, must be implemented as constants)

| Element | default | hover | active/pressed | selected | disabled |
|---|---|---|---|---|---|
| `.side-item` | bg transparent, text/icon `dim` | bg `#1d1d21`, text+icon `text` | — | bg `#1d1d21`, text `text`, lead icon `accent` | — |
| `.ibtn` | color `dim`, bg transparent | bg `#1d1d21`, color `text` | — | `.liked`/`.accent` → `accent` | opacity via row |
| `.card` | bg transparent | bg `#151518` | — | — | opacity .4 + cursor default (offline) |
| `.trow` | bg transparent | bg `#151518` | — | `.playing` bg `#151518`, title `accent`; `.unavailable` opacity .4 | cursor default |
| `.tab` | bg `#151518`, border `#232328`, text dim | text `text` | | `.active` bg+border `accent`, text white | — |
| `.search-btn` | bg `accent`, text white | bg `accent-dim` | | | — |
| `.play-all` | bg `accent`, white | bg `accent-dim` | | | — |
| `.head-btn` | bg raised, border, text | bg `#1d1d21`, border `dim` | | `.downloaded` text accent, border accent-dim; disabled → cursor default, text dim | |
| `.menu-item` | text, transparent | bg `#1d1d21` | | `.danger` text+icon accent | |
| `.btn-primary` | bg accent white | bg accent-dim | | disabled opacity .6 | |
| `.btn-secondary` | bg `#1d1d21`, border | bg `#232328`, border accent | | | |
| `.switch` | bg `#1d1d21` + white knob left | — | | `.on` bg accent, knob translateX(20) | |
| row hover reveals | kebab (`opacity 0→1`), sidebar count↔kebab swap, row download button | | | | |
| focus (`:focus-visible`) | 2 px `#c4a0ff` outline, offset 2; rows/cards/side items/menu options: offset **-2** | | | | |

---

## 3. Icons

The old app draws **filled Material-style SVG icons** (24×24 viewBox, currentColor)
via `ui/src/components/icons.rs`. The native app uses the embedded **Material
Icons** font (`crates/rift-desktop/assets/fonts/MaterialIcons-Regular.ttf`) at the same nominal
box sizes; codepoints (from `MaterialIcons-Regular.codepoints`):

| Icon name in old code | Material Icons glyph | codepoint | Default box | Notes |
|---|---|---|---|---|
| home | home | `e88a` | 16 | sidebar nav |
| heart (filled) | favorite | `e87d` | 16 | liked; np like |
| heart-outline | favorite_border | `e87e` | 16 | not liked |
| heart (sidebar) | favorite (filled) | `e87d` | 16 | sidebar "Liked Songs" lead |
| music | music_note | `e405` | 16 / 30 % of tile | sidebar playlist lead; cover fallback |
| plus | add | `e145` | 16 | New playlist |
| search | search | `e8b6` | 16 | (search box uses text only) |
| play | play_arrow | `e037` | 20 / 16 (play-all) | |
| pause | pause | `e034` | 20 / 16 | |
| prev | skip_previous | `e045` | 20 | |
| next | skip_next | `e044` | 20 / 16 (Play next menu item) | |
| shuffle | shuffle | `e043` | 20 | accent when on |
| repeat | repeat | `e040` | 20 | accent when All |
| repeat-one | repeat_one | `e041` | 20 | accent when One |
| volume | volume_up | `e050` | 17 | |
| x (close) | close | `e5cd` | 14 (window) / 16 (toast/banner/queue remove) | |
| min | remove | `e15b` | 14 | window minimize |
| max | crop_square | `e3c6` | 14 | window maximize |
| trash | delete | `e872` | 16 | delete playlist, clear queue, remove from playlist |
| edit | edit | `e3c9` | 16 | rename |
| download | file_download or download `f090` | 16 | Import playlist, row download, head-btn |
| check | check | `e5ca` | 16 | create/import confirm |
| check-circle | check_circle | `e86c` | 15 | downloaded indicator |
| alert | warning | `e002` | 15 | failed download |
| kebab | more_vert | `e5d4` | 15/16 | row/card/sidebar menus (22–28 px buttons) |
| chevron-down | expand_more | `e5cf` | 14 | queue carets (rotate -90 when collapsed) |
| queue | queue_music | `e03d` | 16 | "Add to queue" menu item |
| person | person | `e7fd` | 16 | "Go to artist" |
| album | album | `e019` | 16 | "Go to album"/Import from file |
| settings | settings | `e8b8` | 16 | Settings nav |

Old code's `*` no icon is shown for track-row duration etc.

**Only the queue collapse caret rotates** (`transform: rotate(-90deg)` when
collapsed). **EQ bars** (`eq_bars`) are 3 × 3 px accent bars, 2 px gap, 14 px
tall, animated 0.9 s ease-in-out bounce between 4 px and 14 px heights, delays
-0.5 s/-0.2 s/-0.7 s; paused = 5 px frozen. Native: animate from `ui.time()`.

---

## 4. Typography

### 4.1 Font faces

Old app: `font-family: "Inter", "Segoe UI", system-ui, sans-serif` with **no
WebFont** — capture machine rendered **Segoe UI**. The screenshot's glyph shapes
are therefore Segoe UI. Native: embedded Inter (Regular/Medium/SemiBold/Bold =
400/500/600/700) from `crates/rift-desktop/assets/fonts/`. If maximum screenshot parity is
wanted, loading `C:\Windows\Fonts\segoeui.ttf` at runtime is an option, but it
is not redistributable; **do not embed it**. Default: Inter + the line-height
table below (which reproduces the Segoe UI boxes).

Weights used: 400 (body/desc), 500 (side items, track titles, np title),
600 (card names, buttons/links, tabs, settings labels, search button),
700 (h2, section labels, queue header, page-kind, play-all, logo? no),
800 (logo wordmark, page titles). Inter assets top out at 700 (Bold): render
weight 800 with Bold; visual difference at 17 px/30 px is negligible.

### 4.2 Size / line-height / color table (native layout must use these line boxes)

| Role | size | weight | line box | color | notes |
|---|---|---|---|---|---|
| body default | 14 | 400 | 18.6 → use **19** | `text` | `html,body` |
| h2 section head | 17 | 700 | **22** | `text` | margins 18 top / 14 bottom (first child: top 0) |
| page title (`h1.page-title`) | 30 | 800 | **40** | `text` | |
| page kind (uppercase) | 11 | 700 | 15 | dim | letter-spacing 1.5, uppercase |
| page sub | 13 | 400 | 17 | dim | margin-top 6 |
| page desc | 13 | 400 | 17 | dim | margin-top 10, max-width 700, 3-line clamp |
| card name | 13 | 600 | **17** | `text` | margin-top 9 (in card) / 0 (in `.card-foot`) |
| card sub | 12 | 400 | **16** | dim | margin-top 2 |
| track title | 13 | 500 | **17** | `text` (accent+600 when `.playing`) | nowrap ellipsis |
| track artist / album | 12 | 400 | **16** | dim | nowrap ellipsis |
| track duration | 12 | 400 | 16 | dim | right-aligned in 40 px |
| sidebar item | 14 | 500 | 19 | dim → text | |
| sidebar count | 11 | 400 | 15 | dim | |
| section label (`PLAYLISTS`, queue head) | 10 | 700 | **13-14** | dim | letter-spacing 1.5; queue head is the same */
| queue row title | 13 | 500 | 17 | text (accent if current) | |
| queue row artist | 12 | 400 | 16 | dim | |
| collection meta | 13 | 400 | 17 | dim | |
| search input / placeholder | 13 | 400 | 17 | text / dim | |
| search button | 13 | 600 | 17 | white | |
| tab | 13 | 600 | 17 | dim → text / white on accent | |
| time (seek) | 11 | 400 | 15 | dim | min-width 34, centered |
| buttons (play-all/head-btn) | 14 | 700/600 | 19 | | |
| settings label | 15 | 600 | 20 | text | |
| settings desc | 13 | 400 | 17 | dim | |
| ytdlp badge | 12 | 600 | 16 | | |
| update banner / toast | 13 | 400 | 17 | | |
| logo wordmark | 17 | 800 | 22 | gradient (fallback accent) | letter-spacing 2 |
| menu item | 13 | 400 | 17 | text | |
| `.empty` state | 13 | 400 | 17 | dim | centered, padding 48 vertical |
| `.link` (artist/album link in rows) | inherit | inherit | inherit | inherit | hover: text + underline |

Text truncation: one line, `ellipsis` (U+2026). Native helper: text module's
`ellipsize`.

`fmt_secs`: `m:ss`, or `h:mm:ss` when ≥ 1 hour. `fmt_total_duration` (collection
subtitle): `"N hr M min"` / `"M min"` / `"S sec"`.

---

## 5. Assets / textures (PaintList has no gradient primitive)

`gfx::paint::Prim` supports solid rounded rects and textured quads only, so
every gradient in the old CSS must be **baked into a small RGBA8 texture** the
UI uploads through `PaintList::created` (screens allocate ids ≥ 1000; atlas uses
low ids). Generate once on first frame, cache the ids in `State`/`UiMemory`:

| Asset | Size | Recipe |
|---|---|---|
| `COVER_FALLBACK` tile | 64×64 (drawn at any cover size; 1:1 aspect) | linear gradient 135° (CSS `linear-gradient(135deg,#1c1c22,#141417)`), alpha 255. UV full. Radius applied by the draw call, not the texture. |
| `LOGO_MARK` | 64×64 PNG (drawn at 32 logical px: a 22 px tile + 5 px glow margin) | Committed prerasterization of the real mark: `crates/rift-desktop/assets/logo/logo.svg` (the legacy logo source), `logo@2x.png` (tile rendered with librsvg/rsvg-convert at 44 px + the CSS `drop-shadow(0 0 3px rgba(157,92,255,.55))` baked as a blurred violet halo), and the reproducible generator `crates/rift-desktop/assets/logo/generate.py`. `screens/cards/textures.rs` embeds the PNG (`include_bytes!`) and draws it at `(x=9, y=7, 32×32)`, i.e. the tile sits at `(14,12)` 22×22. |
| `WORDMARK_GRADIENT` | optional | CSS: `linear-gradient(110deg,#7df9ff 0%,#c084fc 50%,#ff8df5 100%)` clipped to "RIFT". Native approximation: draw each glyph in a tint interpolated along the run: R `#7df9ff`, I `#9f9df1`, F `#e79ce0`, T `#ff8df5` (or a 4-stop ramp). Solid `accent` is the CSS fallback and acceptable if the ramp looks wrong at 17 px. |
| cover art | ≤256×256 | from `controller` cover cache (`Option<TextureId>`); `None` or empty URL → fallback tile + music-note glyph. |

No other image assets are needed. All icons are font glyphs (tinted
`Prim::Image` from the atlas). No shadows except menu/banner/dragged-row shadow
(fake with the renderer's border/rect prims: a 1 px alpha border won't do; if
the paint contract cannot express soft shadows, draw an offset semi-transparent
rounded rect, e.g. menu shadow = rounded rect at +10 y, alpha 0.55 black, radius
8, expanded 0 — acceptable approximation).

---

## 6. Chrome

### 6.1 Titlebar — rect `(0,0,1160,46)`

Background `#111113`, 1 px bottom border `#232328` at y=45. Layout: three groups.

**Logo group** — height 46, `padding: 0 22px 0 14px`, `gap: 9px`, aligned center
horizontally; logo color accent.

* `logo_mark`: 22×22 at **x=14, y=12** (centered vertically). Texture (§5).
  Glow under it (optional: semi-transparent accent rounded rect 22×22 expanded
  ~2 px with blur approximation).
* `logo_text` "RIFT": 17 px / 800, letter-spacing **2 px**, ink was x=45..88,
  y=18..29 (baseline ≈ 29.5). Gradient tint per §5; solid accent fallback.
  Therefore text starts at x = 14+22+9 = **45**.
* The whole titlebar is a **drag region** (`HTCAPTION`); logo group is
  drag-through (mark is non-interactive). Drag = move window; double-click =
  toggle maximize. (Implementation belongs to Lead's window code — screens
  report `WindowDrag` interactions, see §11.)

**Search box** — absolutely centered: `left: 50%; transform: translateX(-50%)`,
`width: min(480px, 42vw)` → **480** at 1160. Measured rect:
**x=340, y=7, w=480, h=31**, radius 6, bg `#151518`, border 1 px `#232328`,
overflow hidden, flex row.

* input: flex 1 (399 px wide), transparent, text 13 px `#ececef`,
  `padding: 7px 12px` → text starts x=353; placeholder
  **"Search YouTube Music..."** in `dim` (ink x=353..496, y=17..26).
  No own focus ring.
* button "Search": flex 0, bg `accent`, white, **padding 0 18px**, 13 px/600 →
  measured x=740..818/819, h=29 (full inner height). Radius: right side follows
  box radius (6). Hover → `accent-dim`. Click/Enter → emit `Search(query)`.
* Focus ring on the wrapper (`:focus-within`): border and 1 px ring
  `#c4a0ff` (box-shadow `0 0 0 1px`).
* Behaviour: typed text triggers a **300 ms debounced** live search that is
  cancelled by a newer keystroke; `Enter` searches immediately and supersedes
  the pending debounce; clearing the box (empty query) emits `SearchClear`
  (which leaves Search view back to Home only if currently on Search).
* Searchbox hover state: none (only focus + button-hover).

**Window controls** — right-aligned; three **46×46** buttons flush to the right
edge (min at x=1022, max at x=1068, close at x=1114).

* Button: transparent, no border, glyph 14×14 centered, color `dim`.
* Hover: bg `#1d1d21`, glyph `text`. Close hover: bg `#e81123`, glyph `#fff`.
* Glyphs: minimize = horizontal 10×2 px line (`remove`); maximize =
  10×10 outlined square (`crop_square`); close = 8×8 X (`close`).
* Actions: minimize → `set_minimized`; maximize/restore →
  `window_toggle_maximize`; close → exit event loop.
* Between the search box and the controls is the flexible drag spacer
  (`.tb-space`).
* **No right-click menu, no title bar text** (decorations off).

States: default only (no disabled). Whole bar is drag region except the search
input/button and the three window buttons.

### 6.2 Sidebar — rect `(0,46,335,610)`

Background `#111113`, 1 px right border `#232328` at x=334 (border-box: width
includes it). `padding: 12px 10px` ⇒ items span x=10..323 (314 px).

Vertical structure (reference capture): scrollable nav area (`flex:1; overflow-y:auto;
scrollbar hidden/no visible scrollbar in shot`), then pinned import rows,
divider, Settings.

Rows (`.side-item`): height **38**, radius 6, `padding: 8px 10px`,
`gap: 10px`, `margin-bottom: 3px`, icon 16×16, text 14 px/500. Default text+icon
`dim`; hover bg `#1d1d21` + text `text`; active bg `#1d1d21` + text `text` +
**lead icon accent**. Long names ellipsize. Right-side count 11 px `dim`.

Measured y positions (top of each 38 px row):

| Element | y | content |
|---|---|---|
| Home | **58** | icon `home` accent (active), "Home" text `text`; ink y=63..90 area |
| Liked Songs | **99** | icon `favorite` dim (or EQ while playing), text "Liked Songs" dim, no count |
| `PLAYLISTS` label | 155 (ink 159..165) | 10 px/700, letter-spacing 1.5, dim; margin `18px 10px 6px`; text x=20 |
| Playlist 1 | **173** | icon `music_note` dim, name "Playlist 1", count "12" right-aligned (right edge 313); ink y=188..202 |
| Playlist 2 | **214** | "Playlist 2", count 9 |
| Playlist 3 | **255** | "Playlist 3", count 16 |
| New playlist | **296** | icon `add`, text "New playlist", color dim (no accent); hover normal |
| Import playlist | **507** (pinned) | icon `file_download`, "Import playlist" dim |
| Import from file | **548** | icon `album`, "Import from file" dim |
| divider | **594** | 1 px `#232328`, `margin: 8px 10px` → x=20..313 |
| Settings | **605** | icon `settings`, "Settings"; active when view == Settings |

Why 507: the three pinned rows are laid out bottom-up from the sidebar's
`padding-bottom: 12`: Settings 605..642, divider 594, Import-from-file
548..585, Import playlist 507..544. The `side-scroll` nav area takes the rest.

Row behaviours:

* Click → navigate. Home → `View::Home`; Liked → `View::Liked`; playlist →
  `View::Playlist(id)`; New playlist → inline draft input; Import playlist →
  inline URL input; Import from file → native file picker command;
  Settings → `View::Settings`.
* Hover (playlist rows): the **count is replaced by a kebab ⋮ button**
  (22×22 ibtn, icon 15) at the right end. Same swap when the row's menu is open
  or when focus is inside the row. Row height does not change.
* Kebab / right-click on a playlist row opens the context menu (positioned
  below-left or below-right; menu spec §6.8) with **Rename, Export, Delete**
  (Delete is `.danger` = accent color in this app). "Rename" starts inline
  rename in the playlist view (the app navigates to the playlist and puts its h2
  into edit mode).
* Drag-reorder playlists: `draggable`; while dragging, rows shift with
  `transform translateY` preview (3 px pitch = row margin), dragged row is
  bg `#151518` + shadow + opacity .92; drop emits `MovePlaylist`. Keyboard
  reorder: **Alt+Up / Alt+Down** on a focused playlist row (list-level handler)
  emits the same move; announcement for a11y is not needed in the native port.
* EQ indicator: when this playlist is the playing source
  (`queue.source == "playlist:<id>"`) the lead icon becomes the 3-bar EQ
  (accent, animated while Playing, frozen 5 px while Paused). Liked row eq when
  source == "liked".
* New-playlist draft: after clicking "New playlist", the row is replaced by an
  input (bg `#151518`, border accent, radius 6, padding 7px 10px, font 13,
  placeholder "Playlist name", width 100 %) with two 22×22 buttons inside on the
  right: confirm (right: 34) and cancel (right: 6), icons `check` / `close`,
  hover bg `#1d1d21` (confirm hover icon accent). Enter confirms (trim, non-empty)
  and emits `NewPlaylist`; Escape or blur cancels without creating.
* Import draft: same UI, placeholder **"YouTube Music link or ID"**, confirm
  emits `ImportPlaylist(url)`; the app shows a **sticky** toast
  "Importing playlist…" until the command resolves, then navigates to the
  imported playlist and shows a notice toast.
* Import-from-file opens a native picker; Enter/Space activates as a button.
* Active row when the current view is Settings: Settings row gets active styles.
* **Resize handle**: 5 px wide strip along the right edge (x=330..334), full
  height, `cursor: col-resize`; hover fill `rgba(139,61,240,0.35)`.
  Drag: width = clamp(pointer_x, **160**, **420**); persisted (old app:
  `localStorage["sidebar-width"]`; native: `State.sidebar_width` persisted
  through settings if cheap). Keyboard: focused separator, Left/Right = ±8
  (Shift = ±32), Home = 160, End = 420.
  **Reference capture: width 335** (see §15).
* When the whole sidebar scrolls (many playlists) it uses a 10 px scrollbar
  (thumb `#2a2a30` r5, track transparent) at the sidebar's right edge inside
  the padding; the pinned bottom block never scrolls.
* Resize handle is *on top of* the sidebar border, is pointer-transparent to
  the rest of the sidebar except its 5 px.

### 6.3 Player bar — rect `(0,656,1160,84)` (1 px top border + 83 content)

Background `#111113`, border-top 1 px `#232328` at y=656. Two rows.

**Seek row** — `y=657..677` (21 px): `padding: 6px 14px 0`, flex, `gap: 10px`,
aligned center.

* time label: 11 px, dim, `min-width: 34px`, text centered. Left label shows the
  current/scrub position (`1:23` in the reference), right label the duration
  (`3:34`). Boxes: x=14..48 and x=1112..1146.
* seek slider: flex 1 → **x=58..1102, height 4**, radius 2. Rendered by the
  native slider widget as: track 4 px `#efefef` (the browser's default
  range-track color — *not* a theme token; keep it literal to match), played
  portion (left of the thumb) `accent` `#8b3df0`, **16×16** circular thumb
  (accent, no border) centered at `x = 58 + 8 + fraction * (1044 - 16)`,
  center y = 670.5. In the reference fraction = 83/214 = 0.3878 → thumb center
  x ≈ 464.7 (measured 464.5, thumb solid 458..471).
  Drag: while dragging, show the dragged value in the left time label and keep
  the thumb at the pointer; on release emit `Seek(position_secs)`.
  (The old app seeds this bar from bootstrap progress and updates it from the
  4 Hz progress events; while `state == Loading` the old app momentarily shows
  the new track's zero position.)
* focus ring: none specified (native may add one).

**Player row** — `y=678..739` (62 px = 6 top + 44 + 12 bottom),
`padding: 6px 16px 12px`, flex, `gap: 16px`, align center.
Regions: `.now-playing` flex:1, `.controls`, `.volume` flex:1 justify-end.
Because `flex: 1` is `1 1 0%`, the two side regions are **442 px each** at
1160: np x=16..458, controls x=474..686, volume x=702..1144. The controls are
therefore centred in the window (play-button centre x = 580); keep that property.

* now-playing: cover **44×44**, radius 5, at x=16, y=684 (fallback tile when no
  cover, music-note glyph at 30 % = 13 px); gap 10; then a two-line text block
  starting x=70 (vertically centred: title line box 17 at y=690.8..707.8, artist
  line 16 below; title 13/500 `text` ellipsized, artist 12 `dim`; artist credits
  are clickable links when they have channel ids (`link` style: hover text +
  underline, no other chrome)); gap 10; **like button** 28×28 ibtn
  (`heart` filled `accent` when liked, `heart-outline` dim otherwise) at
  x≈147.5..175.5 for the preview text (position derives from measured text
  widths; the button hugs the text block, it is not pushed to the controls:
  `.now-playing .trow-meta { flex: 0 1 auto }`).
  When no track is loaded, the whole np region is empty.
* controls: five **36×36** buttons, `gap: 8` → block 212×36, vertically centred
  (y=687.5..723.5; play circle measured 688..723), order:
  1. shuffle (`shuffle`), icon 20; **accent icon when shuffle on**;
  2. previous (`skip_previous`);
  3. **play/pause**: 36×36 circle, bg `text` (`#ececef`), icon 20 in `#111111`;
     hover scales 1.05 (approximate with 2 px larger circle or skip);
     `state == Loading` → pulse opacity 0.4..1 (1 s ease-in-out) and show the
     pause glyph (old behaviour: loading shows `pause`);
  4. next (`skip_next`);
  5. repeat (`repeat`, or `repeat_one` when repeat == One); **accent icon when
     repeat != Off** (all modes use accent; only the glyph differs).
  All icons dim; hover on ibtn → bg `#1d1d21` + icon `text`.
* volume: region x=702..1144, justify-end, `gap: 8`, color dim: `volume_up`
  17×17 icon at x≈1009..1026, then slider **110×4** at x=1034..1144 (same visual
  as seek but 110 wide: track `#efefef`, played fill accent, 16 px thumb, at
  100 % the thumb's right edge is flush with 1144). Vertical centre y ≈ 705
  (thumb 698..713).
  Drag: emits live `SetVolume(v)`; on release emits `SaveVolume(v)` (the old
  app fires set_volume continuously and save_volume once, so moving the slider
  is smooth and the disk write happens once).
* No hover effects on the bar itself. The bar does not scroll.

### 6.4 Queue panel — rect `(335,421,825,235)` when expanded

* top border 1 px `#232328` at y=421; surface `#121215`.
* **header** y=422..465 (**44** = 8 + 28 + 8), `padding: 8px 16px`, flex
  space-between, aligned center:
  * left group `inline-flex gap 6`: caret (`expand_more` 14×14, dim, rotates
    -90° when collapsed) then text **`UP NEXT (N)`** — 10 px/700,
    letter-spacing 1.5 px, dim (ink x=354..443 in reference; caret x=351..365,
    text starts x=371). Hover on header → text `text`.
  * right: **Clear queue** trash button 28×28 ibtn (`delete`, 16) at
    x=1115..1143 (icon ink 1125..1134), hover ibtn style. Click stops
    propagation, emits `QueueClear`.
  * Clicking anywhere else on the header toggles collapse (caret rotates to
    -90°, list hidden). Keyboard: Enter/Space on focused header toggles
    (ignoring key events aimed at the clear button).
* **list** y=466..655 (**190** = max-height incl 8 px bottom padding),
  `padding: 0 8px 8px`, `overflow-y: auto`; content box x=343..1150, 10 px
  scrollbar at x=1150..1159 (thumb `#2a2a30` r5; reference thumb y=466..598).
  Rows: `.qrow` **44** tall each (34 cover + 2×5 padding), `padding: 5px 8px`,
  `gap: 10`, radius 6, full list width (x=343..1142 content):
  * cover `qrow-cover` 34×34, radius 4, x=351 (row pad 8), y=row_top+5;
  * meta: title 13/500 (accent when this row is current), artist 12 dim;
  * hover: bg `#1d1d21`; a 28×28 remove (`close`, 16) ibtn fades in at the
    right end (opacity 0→1 on row hover; keyboard focus also shows it).
  * click row → `QueueJump(index)`; click remove → `QueueRemove(index)`
    (stops propagation). Context: none.
  * drag-reorder: `cursor: grab`; same live-preview/shift mechanics as the
    sidebar (row pitch 44), drop emits `QueueMove{from,to}`; Alt+Up/Down
    keyboard reorder.
  * current row: title accent (`queue.current == Some(i)`), no other marking.
* The queue panel is **hidden entirely when `View::Settings`** and when the
  queue has no tracks. When collapsed only the 45 px header + top border shows
  and main grows.
* When the panel appears/disappears, main's content width must **not** change
  (the 10 px scrollbar gutter in main is always reserved) — this is the whole
  point of `scrollbar-gutter: stable`.

### 6.5 Toasts (error + notice)

Bottom-left floating card, window-anchored: `left: 18px; bottom: 110px;`
`max-width: 420px;` padding **10px 14px**, radius 8, font 13, z above player.
Error style: bg `#2a1216`, border 1 px `#b81e30`, text `#ffb3bb`.
(Information/notice toasts in the old app share this element — the CSS has only
the error palette; the informational text is "Exported …", "Imported …",
"Importing playlist…" and uses the same red-tinted toast. Keep one component;
native styles exactly as above.)
Behaviour: one toast at a time; a newer toast replaces the text and restarts the
timer (error/notice = 5 s, command errors = 6 s, "Importing playlist…" is
sticky until replaced); auto-dismiss after the delay. No close button, no
actions, no wrap beyond max-width (wrap at 420 px). Toast text is plain text.

### 6.6 Update banner

Bottom-right floating card: `right: 18px; bottom: 110px; max-width: 480px;`
flex, align center, `gap: 12px`, padding **10px 12px 10px 16px**, radius 10,
bg `#1c1c20`, border 1 px `#232328`, text `#ececef`, font 13, shadow
`0 8px 24px rgba(0,0,0,.4)`, z above player. Contents, in order:
`Rift v{latest} is available.` text (margin-right 4) → **Download** primary
button (accent, radius 8, padding 8×14, 13/600, white; hover accent-dim; opens
the release URL in the browser) → **Don't ask again** secondary button
(bg `#1d1d21`, border `#232328`, radius 8, padding 8×14, 13/600, text; hover bg
`#232328` + border accent; sets `update_notifications=false` and dismisses) →
close ibtn 28 (`close`, 16) titled Dismiss.
Visible only when the launch update check found a newer release
(`update_available == true`) and the user hasn't dismissed it this session.

### 6.7 Scrollbars (every scroll container)

Width **10 px**, thumb `#2a2a30` radius 5, hover thumb `#3a3a42`, track
transparent. Native widget should draw the thumb over the content (always
visible when the container overflows; no auto-hide animation needed). Reference
thumb positions: main thumb y=46..185 (top of its track; scroll=0), queue list
thumb y=466..598. The main area's 10 px gutter is reserved even when the view
fits. Sidebar scroll region uses the same style but with no visible scrollbar in
the reference capture (content fits).

### 6.8 Popup / context menus

Anchored under the trigger (`top: calc(100% + 4px); left: 0`, or right-aligned
for kebabs in cards/rows: `right: 0`), z above all app chrome; a full-window
invisible backdrop closes it on click/right-click; focus leaving the menu
closes it; Escape closes and returns focus to the trigger.

Visual: bg `#151518`, border 1 px `#232328`, radius 8, padding 4,
**min-width 190 px**, shadow `0 10px 30px rgba(0,0,0,.55)`.
Items: flex, `gap: 10`, `padding: 8px 10px`, radius 6, font 13 `text`,
icon 16 `dim` (flex-shrink 0), label flex 1, hover bg `#1d1d21`.
Separator: 1 px `#232328`, `margin: 4px 6px`. Danger items: text + icon
**accent** (`#8b3df0`, quirk of the old CSS — it reads as the brand color, not
red).
Submenu ("Add to playlist"): the item shows the icon/label plus a caret at the
right (`▸` collapsed / `▾` expanded, 11 px dim); expanding renders the options
under it with `padding-left: 36px`, max-height 200 px overflow-y auto, option
text dim → text on hover, ellipsized names.
Keyboard: focus first item on open; Up/Down/Home/End move; Right expands a
submenu; Left collapses; Enter/Space activates; Escape closes.

Menu contents per context (same order as old UI):

| Trigger | Items |
|---|---|
| sidebar playlist row | Rename · Export · Delete (danger) |
| home playlist card | Rename · Delete (danger) |
| playlist view header | Rename · Delete (danger) |
| track row (kebab, right-click) | Play next · Add to queue · Add to playlist ▸ (only if playlists exist) · Download / Downloading… (inert status) / Remove download (check-circle icon) / Kept offline by a playlist (inert) / Retry download (failed, alert icon) · — separator — · Go to artist (if any artist id) · Go to album (if album id) · — separator — · Remove from playlist (danger, only in playlists) |
| queue row | none (hover X button only) |

### 6.9 Keyboard focus

`:focus-visible` only (mouse users see no ring): 2 px `#c4a0ff` outline,
offset 2 px, radius 4; inset (-2 px) for track rows, queue rows, sidebar items,
cards (and the menu sub-options). Searchbox: wrapper gets border+ring
`#c4a0ff`. Focusable: all buttons, inputs, sidebar rows, cards (via their
overlay button), track/queue rows, list container for Alt+arrow reorder,
resize separator, window controls. Global shortcuts must be suppressed while a
text field has focus (`ui.wants_keyboard()`), exactly like the old app's
"focused control (tabIndex >= 0) wins" rule.

---

## 7. Views

Every view renders inside `main` (clip rect x=335+24-ish; see §1; scrolls
vertically; content starts at main padding top 20). `Settings` is the only view
that hides the queue panel. Scroll position is remembered per view in the old
app only for the main element (a single scroll container: switching views keeps
the scroll offset — actually the old `<main>` element persists across view
changes, so it keeps its `scrollTop`; the native port can keep one scroll
offset for main and reset it on view change if simpler — note as discrepancy).

Common building blocks:

* **h2** — 17/700, line 22, `margin: 18px 0 14px`, first child top margin 0.
* **`.view-head`** — flex row, align center, `gap: 10`, `margin-bottom: 4`;
  contains an h2 (whose own margins are dropped: `margin: 0`) plus optional
  kebab.
* **`.link-btn`** — text action at the far right (`margin-left: auto`), 13/600
  dim, padding 4×6; hover text + underline.
* **`.collection-meta`** — 13 dim: `"N song(s) • H hr M min"`.
* **`.head-actions`** — flex, gap 10, `margin: 18px 0 16px`: the **Play pill**
  (`play-all`: accent bg, white, radius pill, `padding: 8px 22px`, gap 7,
  font 14/700, icon 16; shows Pause icon+label when this collection is the
  current queue and playing/loading; hover accent-dim; click: if already
  current → TogglePlay, else PlayTracks{start}) and the **Download** head-btn
  (see below).
* **`.empty`** — 13 dim, text centered, `padding: 48px 0`.
* **`.page-head`** — flex, gap 22, `margin-bottom: 24`: avatar/cover + meta.
* **`.page-kind`** — 11/700 uppercase, letter-spacing 1.5, dim; margin-bottom 4.
* **`.page-title`** — 30/800, line 40.
* **`.page-sub`** — 13 dim, margin-top 6.
* **`.page-desc`** — 13 dim, margin-top 10, max-width 700, 3-line clamp.
* **`.tabs` / `.tab`** — flex gap 8 margin-bottom 16; pill padding 6×16, radius
  pill, bg raised, border `#232328`, 13/600 dim; hover text; active: bg accent,
  border accent, white.
* **track list** (`.tracklist`) — column of `.trow` rows; see §7.1.3.
* **card grids** — §7.1.2.

### 7.1 Home (`View::Home`)

Source: `ui/src/components/home.rs` + app.rs arm.

**7.1.1 Structure**

1. h2 **"Your library"** (first child → no top margin; line box y=66..88 in the
   reference).
2. `.card-grid`:
   * card #1: **Liked Songs** — cover = first liked track's cover (fallback tile
     when empty library), name "Liked Songs", sub `"{liked.len()} songs"`.
   * one card per playlist, same shape, name = playlist name, sub `"{n} songs"`,
     with a kebab menu (Rename/Delete) revealed on card hover at the right of
     the footer row; whole-card click navigates to the playlist.
   `margin-bottom: 26px`.
3. h2 **"Recently played"**.
4. If `library.recently_played` is empty: `.empty` "Nothing played yet. Search
   for a song to get started." Otherwise the **first 12** recently played tracks
   as a track list (`TrackList`), where a row click plays *that single track*
   (replaces the queue — unlike collection lists which start the collection).

**7.1.2 Card geometry** (measured)

At content width 766: 4 columns of **178** px, gap **18**; cards at
x = 359, 555, 751, 947 (relative to window: 359/555/751/947). Column formula in
§1.1.

* `.card` = the whole clickable tile: `padding: 10`, `border-radius: 8`, transparent
  background, hover bg `#151518` (no border, no shadow), cursor pointer. Card
  height in the screenshot = **222** px: cover 158 + name margin 9 + name line
  17 + sub margin 2 + sub line 16 + padding 20 = 222. First card top y = 102
  (grid top), so cover top = 112 (measured 112..269).
* `.card-cover`: square (`aspect-ratio: 1`, width 100 %), radius 6,
  `object-fit: cover`, bg raised. Fallback (no cover URL): the gradient tile
  (§5) with a centered `music_note` glyph sized **30 % of the cover** (47.4 px
  box; its ink ≈ 31×36 px centered). In the reference the glyph's ink sits
  around x=430..465, y=170..215 inside the first 158 px tile.
* name row: `.card-name` 13/600 `text`, one line ellipsized, x = card + 10
  (369), top = cover bottom + 9.
* sub row: `.card-sub` 12 `dim`, one line, top = name bottom + 2. Text exactly
  `"N songs"` (no pluralization; preview shows "8 songs", "12 songs", …).
* Home playlist cards: the footer is `.card-foot` (flex, gap 6, margin-top 9)
  with text block left and the 24×24 kebab ibtn right; the kebab is invisible
  until card hover or its menu is open; clicking it opens Rename/Delete. The
  card's primary action is an invisible full-card button *under* the kebab.
  Liked Songs card has no kebab.
* No card selected state; hover only.

**7.1.3 Recently played track rows** (shared `.trow`, exact reference metrics)

Row box: x=359..1125 (full content width), height **54** = `padding: 7px 10px`
+ content 40, radius 6. Hover bg `#151518`. Click (except on nested buttons)
plays the track. Right-click/kebab opens the track menu. Focus ring inset.

Row internals (measured, content width 766):

| Cell | x (reference) | size | content |
|---|---|---|---|
| cover | 369 | 40×40 r4 | fallback tile + 30 % (12 px) music note; y = row_top+7 |
| meta | 421..641.5 | flex:1 | title 13/500 line 17 at y=row+10.5 (ink 401..413); artist 12 dim line 16 below (ink 415..426), with per-artist clickable links |
| album | 653.5..722.5 | shrink-to-fit, text centered, max 340 | 12 dim; single line ellipsized; clickable link when album id known (`.link`) |
| spacer | 734.5..955 | flex:1 | empty |
| download slot | 967..995 | 28 wide fixed | offline state: downloaded → `check_circle` 15 accent (button to remove, or plain icon when pinned); downloading → 14 px spinner; failed → `warning` 15 danger; online idle → `download` 15 dim, **opacity 0 until row hover**; offline+not downloaded → empty |
| duration | 1007..1047 | 40 wide, right aligned | 12 dim `m:ss` (empty when unknown) |
| actions | 1059..1115 | like ibtn 28 + kebab ibtn 28 | like heart always visible (filled accent when liked, outline dim when not); kebab `more_vert` 15 hidden until row hover/menu open |

Flex formula: `meta_w = spacer_w = (inner_w - 40 - album_w - 28 - 40 - 56 -
6*12) / 2`, inner_w = content_w - 20. The album column is the album text's own
width (shrink-to-fit, text-align center, ellipsized at 340 max). Rows are 54
tall when the meta text fits; texts never wrap (single line, ellipsis).

States: `.playing` → row bg `#151518` and title accent + weight 600; `.unavailable`
(offline && !downloaded) → row opacity .4, cursor default, click inert;
`.menu-open`/hover revealed controls as above. No selected state.

### 7.2 Liked Songs (`View::Liked`)

1. `.view-head` with h2 **"Liked Songs"** (line 22, no top margin because the
   view-head is main's first child; view-head bottom margin 4).
2. If `library.liked` empty: `.empty` — "No liked songs yet. Click the heart on
   any track." (nothing else).
3. Else: `.collection-meta` `"N songs • {total duration}"`, then `.head-actions`
   with Play pill (source `"liked"`) + Download button, then the track list
   (no remove buttons; source `"liked"` for the playing-collection highlight).

### 7.3 Playlist (`View::Playlist(id)`)

* Not found: `.empty` "Playlist not found."
* Header `.view-head`: h2 with the playlist name + kebab menu (Rename/Delete).
  Rename mode: h2 replaced by `.rename-input` — font 17/700, bg raised,
  border 1 accent, radius 6, padding 4×10, min-width 260, autofocus; typing
  updates the draft; Enter trims and emits `RenamePlaylist` then exits edit;
  Escape or blur exits without saving.
* Empty: `.empty` "Empty playlist. Use ♪+ on any track to add it."
* Else: `.collection-meta`, `.head-actions` (Play pill source
  `"playlist:<id>"`, Download), track list with `on_remove` → the row kebab
  gains "Remove from playlist" (danger) after a separator.

### 7.4 Search (`View::Search`)

Reached by submitting the search box; the query is captured in `search.query`.

1. h2: `Results for “{query}”` (first child → no top margin; uses curly quotes
   U+201C/U+201D).
2. `.tabs` row: **Songs · Artists · Albums**. Active = accent pill.
3. Content by tab:
   * Songs: busy → `.empty` "Searching..."; empty → `.empty` "No results.";
     else the track list (single-track click plays just the song, no source).
   * Artists: lazy-loaded on first open (results cached per query):
     `None` → `.empty` "Searching..."; `Some([])` → `.empty` "No results.";
     else artist card grid.
   * Albums: same, album card grid.
4. Artist cards: same 178 px grid; cover is **round** (`border-radius: 50 %`),
   text centered (`.card-artist`: name and sub both centered), name 13/600,
   sub = subscriber count formatted `"1 subscriber"`, `"12K subscribers"`,
   `"1.2M subscribers"` (compact: one decimal, drop `.0`); missing subscribers →
   empty sub. Card has no kebab.
5. Album cards: cover square (radius 6), name 13/600, sub =
   `"{album_type} • {year}"` when year known else `"{album_type}"`.
6. Search tab result lists replace the list immediately when switching tabs.
7. Clearing the box → `SearchClear`: if current view is Search, navigate Home.
8. A failed search shows an error toast and an empty "No results." list.

### 7.5 Artist (`View::Artist(id)`)

1. Loading: `.empty` "Loading artist..." (nothing else).
2. `.page-head`: circular avatar **150×150** (`border-radius: 50 %`,
   object-fit cover, fallback tile + note glyph), meta block with
   `.page-title` (30/800) = artist name. (The old UI does not render
   `description` or `subscribers` on the artist page even though the type has
   them.)
3. `.view-head` h2 **"Top songs"** + `.link-btn` **"Show all"** (only when
   `tracks_playlist_id` is some) → `View::ArtistSongs(id)`.
4. Track list of `artist_page.tracks` (rows clickable; no remove).
5. If non-single albums exist: h2 **"Albums"** then album card grid.
6. If singles exist: h2 **"Singles"** then album card grid.

### 7.6 Artist songs (`View::ArtistSongs(id)`)

1. `.page-head`: avatar 150 circle + meta:
   `.page-kind` "All songs"; `.page-title` = artist name as a `.link` button
   (navigates back to the artist page); if songs are loaded and non-empty,
   `.page-sub` `"N songs • duration"` and `.head-actions` (Play pill source
   `"artist:<id>"`, Download).
2. Content: `None` → `.empty` "Loading songs..."; empty → `.empty`
   "No songs found."; else track list.
   The full catalogue is fetched on demand and cached per artist id; the header
   reuses the artist page's name/avatar when available.

### 7.7 Album (`View::Album(id)`)

1. Loading: `.empty` "Loading album...".
2. `.page-head`: album cover **170×170**, radius 8; meta:
   `.page-kind` = `"{album_type} • {year}"` (uppercase in CSS) or
   `"{album_type}"`; `.page-title` = album name; `.page-sub` = artist as `.link`
   (when `artist_id` known) followed by `" • N songs • duration"`;
   `.head-actions` (Play pill source `"album:<id>"`, Download) — note the
   actions row lives *inside* the meta block, under the sub line.
3. Track list of album tracks.

### 7.8 Settings (`View::Settings`)

First child h2 **"Settings"** (no top margin), then a vertical stack of
`.settings-row` cards.

Row container: flex, `gap: 20`, `max-width: 640`, `padding: 16px 18px`,
bg `#151518`, border 1 `#232328`, radius 10; `+` 12 px between rows.
Left column `.settings-text` flex 1: `.settings-label` 15/600 text;
`.settings-desc` 13 dim margin-top 4 (can wrap); optional extra controls below.
Right column: control (switch, button) flex-shrink 0.

Rows, in order:

1. **Discord Rich Presence** — desc: "Show the track you're listening to on
   your Discord profile." Right: toggle (`switch` 44×24, radius pill, bg
   `#1d1d21`, on → accent; knob 20 white circle, `translateX(20)` when on).
2. **Crossfade** — desc: "Overlap the end of each track with the start of the
   next for a smooth transition." Right: toggle. When on: below the desc
   `.crossfade-control` (margin-top 10, flex gap 10): slider 160×4 (min 1,
   max 12, step 1; default when toggled on = 6 s) + value label 13 dim
   tabular-nums min-width 28 (`"6s"`). Turning off emits crossfade = 0.
3. **Streaming engine (yt-dlp)** — desc: "Rift uses yt-dlp to fetch audio.
   It's auto-detected from your PATH and common install locations; set a custom
   path below if it lives elsewhere."
   * status line (probe on mount; `None` → 13 dim "Checking…"):
     found → green badge "✓ Found" (`#4ade80` on `rgba(74,222,128,.12)`,
     padding 2×8, radius pill, 12/600) + version 13 dim + `<code>` path
     (12 dim, bg `#0e0e10`, border 1 `#232328`, radius 6, padding 1×6,
     word-break all);
     missing → red badge "✗ Not found" (`#f87171` on `rgba(248,113,113,.12)`)
     + detail text: "Not found on your PATH or in common install locations."
     or "Found a file at {path} but it didn't run."
     lines: flex wrap, gap 8, margin-top 10.
   * if missing: `.ytdlp-actions` (flex gap 10, margin-top 10):
     primary button "Download yt-dlp" (accent; disabled while downloading →
     label "Downloading…") + hint 12 dim "Fetches the latest build into Rift's
     data folder."
   * always: `.ytdlp-custom` (flex gap 8, margin-top 10): text input (flex 1,
     max-width 360, bg `#0e0e10`, border `#232328`, radius 6, padding 7×10,
     13 px, placeholder "Custom yt-dlp path (leave blank to auto-detect)",
     focus border accent) + secondary button **"Save"** (re-probes; blank
     clears the override).
   * right column: secondary button **"Check again"**.
4. **Updates** — desc: "Check GitHub for a newer version of Rift on launch and
   notify you."
   * status line: `None` → "Checking…" dim; update available → red badge
     "Update available" + dim `"v{current} → v{latest}"`; up to date → green
     badge "✓ Up to date" + dim `"v{current}"`; failed (latest None) → dim
     "Rift v{current} — could not reach GitHub".
   * `.ytdlp-actions`: secondary "Check now" + (when available) primary
     "Download update" (opens release URL).
   * right column: update-notifications toggle.

### 7.9 States inventory (every screen)

| State | Where | Rendering |
|---|---|---|
| search busy | Search songs | `.empty` "Searching..." |
| search empty | Search songs/artists/albums | `.empty` "No results." |
| artist/album loading | Artist/ArtistSongs/Album | `.empty` "Loading artist..." / "Loading songs..." / "Loading album..." |
| page load failed | any page | error toast (6 s) + the loading/empty state remains |
| liked empty | Liked | `.empty` "No liked songs yet. Click the heart on any track." |
| playlist empty | Playlist | `.empty` "Empty playlist. Use ♪+ on any track to add it." |
| playlist missing | Playlist | `.empty` "Playlist not found." |
| home recently empty | Home | `.empty` "Nothing played yet. Search for a song to get started." |
| queue empty | all | queue panel not rendered |
| queue collapsed | all except Settings | 45 px header only, caret -90° |
| offline | rows/cards | `.unavailable` opacity .4 (un-downloaded track rows unplayable) |
| downloading | track row dl slot | 14 px spinner (2 px border dim, top transparent, 0.7 s spin) |
| downloaded | track row dl slot | `check_circle` accent 15 (row button remove; pinned = plain icon) |
| download failed | track row dl slot | `warning` 15 danger, click retries |
| downloaded collection | head-btn | "Downloaded" accent text/border; disabled when pinned by another playlist; click removes |
| downloading collection | head-btn | disabled, spinner + "Downloading n/total" (n = done+1 clamped) |
| playing collection | play pill | shows "Pause" + pause icon and toggles |
| window maximized | titlebar | maximize glyph unchanged in old app (no restore glyph) |
| update available | banner | §6.6 |
| toast | window | §6.5 |
| hover, focus, active | everywhere | §2.4 / §6.9 |

---

## 8. Interactions master table

| Interaction | Target | Effect |
|---|---|---|
| click | Home/Liked/playlist/Settings sidebar row | navigate |
| hover | playlist sidebar row | count → kebab; kebab opens menu |
| right-click | playlist sidebar row | open menu at pointer (Rename/Export/Delete) |
| drag | sidebar playlist row | reorder with live preview; drop emits MovePlaylist |
| Alt+Up/Down | focused playlist sidebar row | reorder |
| drag | sidebar right edge (5 px) | resize 160..420, hover tints accent 35 % |
| drag | titlebar (non-control area) | window drag; double-click toggle maximize |
| click | min/max/close | window actions |
| type | search box | debounce 300 ms → search; Enter → search now; empty → clear |
| click | Search button | search now |
| click | tab | switch search tab (lazy load artists/albums) |
| click | card | navigate (Liked/Playlist/Album/Artist) |
| hover | card | bg raised; playlist card reveals kebab |
| click | card kebab | menu Rename/Delete |
| click | track row | play (collection start index / single track) |
| hover | track row | bg raised; reveal kebab + download slot button |
| click | heart | toggle like (stops row click) |
| click | kebab | open track menu (stops row click) |
| right-click | track row | open track menu |
| click | album/artist link in row | navigate (stops row click) |
| click | play pill | play collection / toggle pause if current |
| click | download head-btn | download collection / remove downloads |
| click | queue header | collapse/expand (caret rotates, height 45/235) |
| click | queue clear | clear queue |
| click | queue row | jump to that index |
| hover | queue row | bg hover; reveal remove X |
| click | queue remove | remove that index (stops jump) |
| drag | queue row | reorder with preview; drop emits QueueMove |
| drag | seek slider | scrub (label + thumb follow); release → Seek |
| drag | volume slider | live volume; release → SaveVolume |
| click | like (player) | toggle like current track |
| click | shuffle / repeat | toggle shuffle / cycle repeat Off→All→One |
| click | prev/next/play | transport commands |
| click | toast | nothing (auto-dismiss) |
| click | banner Download / Don't ask again / X | open URL / silence+dismiss / dismiss |
| keys | Space / Ctrl+← / Ctrl+→ | toggle play / prev / next, suppressed while a text field has keyboard |
| keys | menu | ↑/↓/Home/End move; →/← submenu; Enter/Space activate; Esc close |
| keys | search | Enter submit; Esc in menus etc. |
| keys | inline inputs | Enter commit, Escape cancel, blur cancel |
| wheel | main / sidebar / queue / menu submenu | scroll 10 px scrollbar containers |

Cursor changes: pointer on all clickables (buttons, rows, tabs, links, toggles and
sliders); text on text inputs; `ResizeH` on the sidebar resize handle (mapped to
the OS `ew-resize` icon); reorderable queue/sidebar rows show the pointer on
hover and switch to `grabbing` only while a reorder drag is actually held — the
old webview UI's grab-on-hover is not reproduced. (Native: winit `set_cursor`
through the UI's `CursorShape`; low priority for the screenshot.)

---

## 9. Preview data (`RIFT_PREVIEW=1`) — port of `ui/src/app.rs`

Already being ported by Lead to `crates/rift-desktop/src/preview.rs`; this is the exact spec the
screenshot depends on. `preview_library()`, `preview_track(n)`,
`preview_queue()`:

```rust
fn preview_track(n: usize) -> Track {
    Track {
        id: format!("preview-{n}"),
        title: format!("Song Title {n}"),
        artist: "Artist Name".into(),
        album: Some("Album Name".into()),
        duration: Some(150 + (n as u32 * 37) % 120),  // n=1 →187 (3:07), n=2 →224 …
        cover: String::new(),                        // always the fallback tile
        artists: Vec::new(),
        album_id: None,
    }
}

fn preview_library() -> Library {
    Library {
        liked: (1..=8).map(preview_track).collect(),                 // "8 songs"
        playlists: vec![
            Playlist { id: "preview-pl-1", name: "Playlist 1",
                       tracks: (1..=12).map(preview_track).collect() },  // "12 songs"
            Playlist { id: "preview-pl-2", name: "Playlist 2",
                       tracks: (13..=21).map(preview_track).collect() }, // "9 songs"
            Playlist { id: "preview-pl-3", name: "Playlist 3",
                       tracks: (22..=37).map(preview_track).collect() }, // "16 songs"
        ],
        recently_played: (1..=12).map(preview_track).collect(),
    }
}

fn preview_queue() -> QueueSnapshot {
    QueueSnapshot { tracks: (1..=6).map(preview_track).collect(),
                    current: Some(0), shuffle: false,
                    repeat: RepeatMode::Off, source: None }
}
```

Plus the preview overrides in the app shell:

* current track = `Some(preview_track(1))` ("Song Title 1", "Artist Name"),
* progress = `Progress { position: 83.0, duration: 214.0 }` → times **1:23 /
  3:34** and thumb at 38.8 %,
* playback state = `Playing` → pause glyph on the player button; no playing
  collection highlight (queue.source None → no sidebar/collection accent),
* volume = **1.0** (thumb at far right),
* liked lookup: `preview_track(1)` is in `liked` → player heart filled accent;
  first recently-played row heart filled accent,
* sidebar counts: Playlist 1 = 12, Playlist 2 = 9, Playlist 3 = 16,
* view = Home, no toast, no update banner, no menus,
* **sidebar width 335** (capture used a resized sidebar; default is 216 — see
  §15),
* no window maximized; window exactly 1160×740.

Reference duration strings for the 12 recent rows (n = 1..12): 3:07, 3:44, 4:21,
2:58, 3:35, 4:12, 2:49, 3:26, 4:03, 2:40, 3:17, 3:54.

The reference screenshot also captures a **hover state**: the mouse is over the
first Recently played row, so that row is painted with `#151518` and — because
preview playlists have no content beyond track metadata — its kebab and download
buttons are hover-revealed (kebab visible at the far right of the row, download
`download` icon visible in the download slot). Reproduce this by optionally
setting the pointer position in the screenshot harness (e.g.
`--pointer 700,407`) or by having `--preview --screenshot` inject a hover at the
first recent row. Reference row y band 386..440; a pointer at (700, 407) does it.

---

## 10. State model (legacy app → native `State`)

Old `ui/src/app.rs` owns all state via `use_state`/`use_mut_ref`. Native
equivalent lives in `crates/rift-desktop/src/state.rs` (Lead + controller own it); screens get
`&State` and push `Action`s. Mapping:

### 10.1 Persistent fields

| Field | Type | Updated by | Consumed by |
|---|---|---|---|
| `library` | `Rc<Library>` / `Library` | bootstrap; `Library` event; (optimistic after commands not needed — backend emits) | sidebar (playlists+counts), home cards/recent, Liked, Playlist, menus (playlist options), playlist fully-downloaded checks |
| `queue` | `QueueSnapshot` | bootstrap; `Queue` event | player bar (shuffle/repeat), sidebar EQ source, play pill "is current/Pause", queue panel rows/current/collapse source |
| `playback_state` | `PlaybackState` | bootstrap; `State` event | player bar play/pause glyph+loading pulse, sidebar EQ animation state, play pill |
| `track` | `Option<Track>` | bootstrap; `Track` event (set Some; `Stopped` handled via state) | player bar np block/heart, home like states (via liked index), EQ visible source |
| `progress` | `Progress` | bootstrap seed; `Progress` event (~4 Hz) | player bar seek position/duration (old app keeps live progress **inside PlayerBar only** so the rest of the UI doesn't re-render 4×/s; native can keep it in State — repaint is per frame anyway) |
| `volume` | `f32` | bootstrap; slider (live) | player bar volume slider |
| `discord_rpc` | `bool` | bootstrap; settings toggle | settings |
| `crossfade` | `f32` | bootstrap; settings toggle/slider | settings |
| `yt_dlp_path` | `Option<String>` | bootstrap; save path response | settings input draft (re-synced when it changes) |
| `update_notifications` | `bool` | bootstrap; settings/banner silence | banner display gate, settings toggle |
| `update_banner` | `Option<UpdateStatus>` | launch check result; dismiss/silence | update banner |
| `update_status` | `Option<UpdateStatus>` | launch check, "Check now" | settings status line |
| `view` | `View` | nav actions, search submit, backend `OPEN_PLAYLIST` event, delete-playlist fallback | root layout, sidebar active states, queue-vs-settings, main content |
| `search` | `{ query, results: Vec<Track>, busy }` | search box submit/debounce; search result/error (guarded by generation) | Search view |
| `search_tab` | `Songs\|Artists\|Albums` | tab click; reset on new search | Search view |
| `artist_results` | `Option<Vec<ArtistSummary>>` | lazy fetch per query (generation-guarded) | Search artists tab |
| `album_results` | `Option<Vec<AlbumSummary>>` | lazy fetch | Search albums tab |
| `artist_page` | `Option<ArtistPage>` | `get_artist` on view enter (generation-guarded); cleared first | Artist view, ArtistSongs header |
| `artist_songs` | `Option<(String, Vec<Track>)>` | `get_artist_songs` on demand, cached by artist id | ArtistSongs view |
| `album_page` | `Option<AlbumPage>` | `get_album` on view enter | Album view |
| `downloads` | `DownloadState` | bootstrap; `Downloads` event | rows' dl slot, head-btn state, pinned logic, menu labels |
| `preview` | `bool` | bootstrap | shell swaps library/track/queue/progress (debug only) |
| `online` | `bool` | bootstrap initial + network reachability events/probe | rows' `.unavailable` |
| `renaming` | `Option<(playlist_id, draft)>` | kebab Rename; inline input; Enter/Escape/blur | Playlist view header |
| `toast` | `Option<String>` + generation counter | error/notice events, frontend failures, sticky import; timer dismiss | toast overlay |
| `search_generation`, `page_generation` | `u64` counters | bumped on every new search/navigation | stale-response guards |
| `sidebar_width` | `f32` (160..420, default 216) | drag/keyboard resize; persisted | sidebar layout; `--screenshot` uses 335 |
| `queue_collapsed` | `bool` | header click | queue panel height |
| `scroll_offsets` | per-container f32 | wheel/drag scrollbar | main/sidebar/queue lists |
| `cover_textures` | `HashMap<url, Option<TextureId>>` | cover loader | cards/rows/player covers |
| `ui_memory` | popup/menu state (which menu open, which id), focus, text-edit state, drag state | interactions | all widgets |
| `pointer_hover` (implicit) | from `InputState` | every frame | hover styling; screenshot harness can pin it |

### 10.2 Playlist fully-downloaded / pinned rule (used by rows and head buttons)

`fully_downloaded(p) = !p.tracks.is_empty() && all(p.track.id in downloads.downloaded)`.
A track id is "pinned" when some fully-downloaded playlist contains it. Row dl
slot shows a non-actionable check when `downloaded && pinned`; removing a
download is blocked if it would break another playlist's offline copy (the old
app shows a toast naming the playlists). Collection Download button state:
downloading (n_active>0) → disabled spinner + "Downloading {min(done+1,total)}/{total}";
all downloaded → "Downloaded"; if any pinned by *another* playlist → disabled
with title tooltip; else clickable.
Do not over-invest here in the first native pass — the screenshot has no
downloads; keep the rules for behaviour parity but they can land after M5.

### 10.3 Local component state in the old UI (native equivalents)

| Old (component-local) | Native home |
|---|---|
| PlayerBar `scrubbing` (drag position) + `live` progress + track-change reseed | player bar local state in `App::ui_memory` or a small `PlayerBarState` in State; when track id changes, reset elapsed=0/duration=new track's duration until the backend emits its first progress tick |
| Sidebar `creating`/`importing` draft modes, input node | `UiMemory` transient input states + action sink |
| Sidebar/reorder drag position, hover-calm | `UiMemory` drag state; native pointer only |
| Menu `open`/`submenu expanded` per trigger | `UiMemory.active_menu: Option<MenuId>` + expanded action index |
| QueuePanel `collapsed` | `State.queue_collapsed` (per-session) |
| Settings `ytdlp` probe/downloading/path draft/update check on mount | local cache in State or a per-view `SettingsState`; probe once on entering Settings and on "Check again" |
| titlebar debounce token | State: `search_debounce: { text, deadline, generation }` driven by frame time |

### 10.4 Effects / subscriptions the controller must provide

Old app's `use_effect_with` subscriptions (all once at startup except page
loads):

1. once: subscribe `LIBRARY`, `QUEUE`, `STATE`, `TRACK`, `DOWNLOADS`, `ERROR`,
   `NOTICE`, `OPEN_PLAYLIST`; install frontend error sink; invoke `bootstrap`;
   after bootstrap, if `update_notifications` → async `check_update`, show
   banner when `update_available`.
2. once: global keydown shortcuts (Space / Ctrl+arrows) skipped when a control
   is focused.
3. once: network online/offline tracking.
4. on `view` change: `Artist(id)` → clear + fetch `get_artist`; `ArtistSongs(id)`
   → use cache if same artist else clear + fetch `get_artist_songs`; `Album(id)`
   → clear + fetch `get_album`; bumped `page_generation` invalidates stale
   responses.
5. PlayerBar: subscribe `PROGRESS` for live ticks (native: controller exposes
   progress events; State applies them).
6. Settings mount: probe `check_ytdlp`; if no cached `update_status`, run
   `check_update`.
7. Toast timers (auto-dismiss; a generation counter prevents an older timer
   from clearing a newer toast).

---

## 11. Actions screens emit (proposal — Lead/controller owns the enum)

Navigation/UI-local (controller applies synchronously): `Navigate(View)`,
`OpenArtist(id)`, `OpenAlbum(id)`, `OpenArtistSongs(id)`, `SearchTab(tab)`,
`SearchClear`, `ToggleQueueCollapsed`, `SetSidebarWidth(f32)`,
`DismissUpdateBanner`, `SilenceUpdateNotifications`, `OpenMenu(MenuId)`,
`CloseMenu`, `Focus(id)`, `Scroll(id, dy)`.

Backend commands (old names in parentheses):

| Action | old command |
|---|---|
| Search(query) | `search` |
| SearchArtists(query) / SearchAlbums(query) | `search_artists` / `search_albums` |
| PlayTrack(track) | `play_track` |
| PlayTracks{tracks, start, source} | `play_tracks` |
| TogglePlay / NextTrack / PrevTrack | `toggle_play` / `next_track` / `prev_track` |
| Seek(f64) | `seek` |
| SetVolume(f32) / SaveVolume(f32) | `set_volume` / `save_volume` |
| ToggleShuffle / CycleRepeat | `toggle_shuffle` / `cycle_repeat` |
| ToggleLike(track) | `toggle_like` |
| QueueAdd(track) / QueuePlayNext(tracks) / QueueJump(i) / QueueRemove(i) / QueueMove{from,to} / QueueClear | `queue_add` / `queue_play_next` / `queue_jump` / `queue_remove` / `queue_move` / `queue_clear` |
| AddToPlaylist{id,track} | `add_to_playlist` |
| CreatePlaylist(name) | `create_playlist` |
| RenamePlaylist{id,name} / DeletePlaylist(id) / MovePlaylist{id,to} | `rename_playlist` / `delete_playlist` / `move_playlist` |
| RemoveFromPlaylist{id,track_id} | `remove_from_playlist` |
| ImportPlaylist(url) / ImportFile / ExportPlaylist(id) | `import_yt_playlist` / `import_playlist` / `export_playlist` |
| DownloadTracks(tracks) / RemoveDownloads(ids) | `download_tracks` / `remove_downloads` |
| SetDiscordRpc(b) / SetCrossfade(f32) | `set_discord_rpc` / `set_crossfade` |
| SetUpdateNotifications(b) / CheckUpdate | `set_update_notifications` / `check_update` |
| CheckYtDlp / SetYtDlpPath(s) / DownloadYtDlp | `check_ytdlp` / `set_yt_dlp_path` / `download_ytdlp` |
| OpenUrl(url) | `open_url` |
| LoadPage{kind,id} (artist/artist_songs/album) | `get_artist` / `get_artist_songs` / `get_album` |
| Window(Minimize \| ToggleMaximize \| Close \| StartDrag \| StartResize(edge)) | former window commands |

---

## 12. ASCII layout diagrams (reference: sidebar 335, 1160×740)

```
┌────────────────────────────────────────────────────────────────────────────────────────┐ y=0
│ [logo 14..36] RIFT(45..88)      ┌───────────────────────────────────────────────┐      │
│                                 │ Search YouTube Music...      [  Search  ]     │      │  titlebar 46
│                                 │ 340..740 raised   740..819 accent (h=29 y=7)  │      │
│                                 └───────────────────────────────────────────────┘      │
│                                                              [– 1022][□ 1068][× 1114]  │
├─────────────────────────┬──────────────────────────────────────────────────────────────┤ y=45 border, 46 body
│ Home        58..95 38px │ Your library                                           y=71 ink│
│ Liked Songs 99..136     │ ┌──────┐ 18 ┌──────┐    ┌──────┐    ┌──────┐              │
│ PLAYLISTS   155 ink     │ │cover │    │cover │    │cover │    │cover │  cols 178     │
│ Playlist 1  173 count12 │ │158²  │    │158²  │    │158²  │    │158²  │  gap 18       │
│ Playlist 2  214 count 9 │ └──────┘    └──────┘    └──────┘    └──────┘  cover y112    │
│ Playlist 3  255 count16 │ Liked Songs Playlist 1 Playlist 2 Playlist 3  name y281     │
│ New playlist 296        │ 8 songs     12 songs   9 songs    16 songs    sub y300      │
│                         │ (card 178×222, grid bottom 324, margin-bottom 26)            │
│                         │ Recently played                                            │
│                         │ ░hover░ Song Title 1     Album Name        3:07 ♥    y386..440
│                         │ (clipped by queue panel at y=421)                           │
│ Import playlist 507     ├──────────────────────────────────────────────────────────────┤ y=421 queue border
│ Import from file 548    │ ▾ UP NEXT (6)                                            🗑   │ head 44
│ ─────── 594 ───────     │ [cov] Song Title 1   Artist Name                     y471  │ list 190
│ Settings    605..642    │ [cov] Song Title 2   Artist Name                            │
│                         │ … 6 rows of 44 px, scrollbar 1150..1159                    │
├─────────────────────────┴──────────────────────────────────────────────────────────────┤ y=655
│ 1:23  ●───────────────────────────────────────────────────  3:34                     │ seek row 21
│            (seek track 4px y669..672, thumb 16px @464.5)                              │
│ [44²] Song Title 1   ♥      ⤨  ⏮  ▶  ⏭  ⟳            🔊 ▬▬▬▬▬▬▬▬▬▬●               │ player row 62
│      Artist Name                                                                       │
└────────────────────────────────────────────────────────────────────────────────────────┘ y=740
```

Sidebar item internal (38 px row, x=10..323):
```
 x: 10  ┌─ radius 6, bg #1d1d21 if active/hover ─────────────────────┐ 323
        │ 10 │ 16px icon (20..36) │ 10 gap │ text 14px (46..)   ... count right 313 │
 y:     └────────────────────────────────────────────────────────────┘
```

Track row (54 px, content width 766, x=359..1125):
```
│10│ cover 40² (369) │12│ meta flex1 (421..641) │12│ album shrink (653..722) │12│ spacer flex1 │12│ dl 28 (967..995) │12│ dur 40 right (1007..1047) │12│ like28 kebab28 (1059..1115) │10│
```

Player row (62 px):
```
│16 │ np cover 44 (16..59) │10│ meta(text) │10│ like 28 │16│ controls 212 centered (474..686) │16│ volume region flex1 justify-end (…1144) │16│
```

Titlebar (46 px):
```
│14 logo22│9│ RIFT 17/800 │22│ …flex drag… │ searchbox 480 centered (340..819, y7..37) │ …flex… │ win buttons 3×46 (1022..1159) │
```

---

## 13. `docs/vulkan-migration/reference-old-ui.png` region by region (acceptance target)

Image is exactly 1160×740, sRGB, no window chrome (decorations off), dark theme.
Captured with RIFT_PREVIEW=1, sidebar width 335, mouse hovering the first
Recently played row (that row's hover background + revealed controls are part of
the image).

1. **Titlebar (y 0–45).** Near-black bar (`#111113`). Left: violet/teal cosmic
   square logo at (14,12) 22×22 with a soft glow, then the wordmark "RIFT" in a
   cyan→violet→pink gradient (capital letters, wide letter-spacing). Center:
   480 px wide search field, 31 px tall, dark raised background, 1 px border,
   placeholder "Search YouTube Music..." in grey, and a solid violet "Search"
   button (80 px wide) at its right end. Right: three flat 46 px window
   buttons — minimize bar, maximize square, close X — in grey glyphs. A 1 px
   `#232328` line closes the band.
2. **Sidebar (x 0–334, y 46–655).** `#111113`. Rounded "Home" row highlighted
   (`#1d1d21`) with a violet home icon and white label. Below, grey "Liked
   Songs" with a heart outline. A tiny letter-spaced "PLAYLISTS" label, then
   three playlist rows (music note, name, right-aligned count 12/9/16) in grey,
   then "+ New playlist". At the bottom (above a divider): "Import playlist"
   (download icon), "Import from file" (disc icon), a 1 px divider, and
   "Settings" (gear). A 1 px border separates the sidebar from content.
3. **Main (x 335–1149, y 46–420 visible).** `#0e0e10`. "Your library" heading
   (bold 17 px) at top-left; a row of **four** equal cards starting at x=359:
   178 px wide tiles with 158 px rounded cover placeholders (subtle dark
   diagonal gradient, centered grey music note), then bold names "Liked Songs",
   "Playlist 1", "Playlist 2", "Playlist 3" and dim subtitles "8 songs",
   "12 songs", "9 songs", "16 songs". Below: heading "Recently played"; the
   first track row is **hovered** (raised background): 40 px cover placeholder,
   violet "Song Title 1" (playing? no — the hover row is not the playing row;
   the title renders violet here because… **check**: in the reference the first
   recently-played title "Song Title 1" is *violet*, matching `.playing`; but
   `queue.source` is None in preview data and `playing_id` is `preview-1`, which
   **is** the current track id, so `.trow.playing` applies. The accent title is
   therefore correct: the row is the currently playing track, not playing
   collection source.) Right side: "Album Name" centered, duration "3:07", and
   a filled violet heart; kebab/download controls revealed by hover. The row is
   clipped by the queue panel after ~34 px.
4. **Queue panel (x 335–1159, y 421–655).** Slightly lighter than main
   (`#121215`), 1 px top border. Header: small chevron + "UP NEXT (6)" in
   letter-spaced grey caps, trash-can "clear" icon at the right. List of six
   songs; rows visible: Song Title 1..4 fully-ish and Song Title 5 cut by the
   panel bottom. Each row: 34 px cover placeholder, white title, grey artist
   "Artist Name"; the first (currently playing) row's title is **not** accent in
   the image because the queue row's `.qrow.current` accent also applies only to
   `trow-title`… it *is* accent in the image (violet). A 10 px scrollbar thumb
   is visible at the far right (y 466–598).
5. **Player bar (y 656–739).** `#111113`, 1 px top border. Top row: "1:23" on
   the left (11 px grey), a full-width seek slider with a violet played portion
   and a white unplayed track, violet 16 px round thumb at ~39 %, "3:34" on the
   right. Bottom row: 44 px cover placeholder with music note; "Song Title 1"
   white 13 px over "Artist Name" grey 12 px; violet filled heart right after
   the text; five transport controls centered — grey shuffle, grey previous,
   **white circular 36 px play button with dark pause glyph**, grey next, grey
   repeat; at the far right a grey volume icon and a 110 px volume slider filled
   violet to the end (volume 100 %).
6. **Overlays.** None visible (no toast, no update banner, no menu).
7. **Scrollbars.** Main area: 10 px thumb `#2a2a30` at the far right from the
   top of main to y≈185. Queue list: thumb at the far right y≈466–598.

Pixel anchors to check (x,y → expected color): (0,0) `#111113`; (340,22) border
`#232328`; (500,7) border `#232328`; (779,22) accent `#8b3df0`; (334,300)
border `#232328`; (10,60) `#1d1d21` (Home active); (369,200) cover gradient
(~`#191920`); (500,420) queue bg `#121215`; (700,407) `#151518` (hovered row);
(1155,100) `#2a2a30` (main scrollbar); (580,705) `#ececef` (play button);
(464,670) accent (seek thumb); (1037,705) accent (volume fill); (0,739)
`#111113`; (1159,739) `#111113`.

Note on the **recently-played row title**: `TrackListCtx.playing_id` is the
current `track.id` = `preview-1`, so the first recent row is `.playing` (bg
raised **and** title accent). This is why in the image the row background is
`#151518` — part of it is hover, part is `.playing`; both set the same color, so
the screenshot is consistent whether or not the pointer was over the row. The
hover-revealed kebab/download icons in the image are the only pointer evidence.

---

## 14. Acceptance checklist (M5)

`cargo run -p rift-desktop -- --preview --screenshot target/preview.png --size 1160x740`
then compare to `docs/vulkan-migration/reference-old-ui.png`:

- [ ] Band edges exactly y=45/46 (titlebar border), y=420/421 (queue border),
      y=655/656 (player border); player 84 px; titlebar 46 px.
- [ ] Sidebar 335 px wide (screenshot/preview mode), 1 px border, Home row
      active at y=58 h=38, playlist rows at 173/214/255, counts 12/9/16.
- [ ] Search box 480×31 at (340,7); accent button 80 px; placeholder text.
- [ ] Window buttons hit the right edge: 3 × 46, glyph centres 1044.5/1090.5/1136.5.
- [ ] "Your library" ink starts ~y=71; 4 cards at x=359/555/751/947, covers
      158×158 at y=112, names ~y=281, subs ~y=300.
- [ ] "Recently played" heading ink ~y=355; first row top y=386, 54 px,
      cover 40 at (369,393), title ink ~y=401, album column centred ~x=688,
      duration right ~x=1045, heart ~x=1072.
- [ ] Queue panel header text "UP NEXT (6)" ink y≈441; 6 rows of 44 from y=466;
      scrollbar thumb at x=1150.
- [ ] Seek times at y≈663–677; track 4 px y=669–672; thumb centre (464.5,670.5).
- [ ] np cover 44 at (16,684); title ink (70,694); controls at 474..686 with
      white 36 px play button at 562..597; volume icon ~1009, slider 1034..1144.
- [ ] Colors: every large region matches the token table exactly.
- [ ] No anti-aliasing seams between bands; 1 px borders land on exact rows.

---

## 15. Discrepancies / requests for other owners

1. **`crates/rift-desktop/src/theme.rs` token corrections** (Lead owns the file). The file's
   current defaults disagree with `styles.css`/the screenshot:
   * `player_height`: **84** (not 92).
   * `queue_height`: **235** expanded (not 216); add
     `queue_collapsed_height = 45` (not 34).
   * `sidebar_width_max`: **420** (not 400).
   * `queue_row_height`: 44 is right; `track_row_height` 54 is right.
   * Add tokens/constants for: titlebar search box (480×31), seek track 4 with
     16 px thumb, scrollbar width 10, sidebar item height 38 + margin 3, card
     tile padding 10 / name margin 9 / sub margin 2, main gutter 10.
   * Add the two bar background colors already present; add `cover_fallback`
     gradient stops (`#1c1c22`, `#141417`), toast colors, banner background
     `#1c1c20`, scrollbar thumb `#2a2a30`.
2. **Reference sidebar width = 335.** The old app's default is 216 (CSS
   `width: 216px`, persisted `localStorage["sidebar-width"]`), but
   `docs/vulkan-migration/reference-old-ui.png` was captured with a user-resized sidebar of 335. To pass
   the screenshot comparison, the preview/screenshot path should use **335**
   while the interactive default stays 216. Decision needed from Lead. (Every
   measured x in §13 is based on 335; §1.1 gives the formulas for other widths.)
3. **Font.** Old capture used Segoe UI (no Inter installed, no `@font-face`).
   Native embeds Inter. Glyph shapes will differ slightly; use §4.2 line boxes
   so the *layout* matches. Optionally load the Windows Segoe UI face at
   runtime for a closer screenshot; do not embed it (license).
4. **Gradients need textures** (§5): cover-fallback tile and the logo mark
   cannot be expressed with `Prim::Rect` alone. Needs either (a) a
   `Ui::image`/texture upload path available to screens (already present:
   `Ui::upload_texture` + `PaintList::created`; screens allocate ids ≥ 1000), or
   (b) renderer support for a gradient primitive (not planned). The `imgui`
   agent should expose a tiny `image(rect, texture, uv, tint, radius)` widget.
5. **`Ui::wants_keyboard()`** currently returns `false` in the stub; the global
   Space/Ctrl-arrow shortcuts depend on it (must become "a text field has
   focus"). `screens` should not implement shortcuts itself — Lead's `app.rs`
   does.
6. **Screenshot hover injection** — *resolved*: `main.rs` grew a `--pointer x,y`
   flag and preview `--screenshot` defaults to `(700,407)`, so the first
   Recently-played row paints its hover background and revealed controls.
7. **`crates/rift-desktop/src/screens` contract — decided.** Root composition lives in
   `crates/rift-desktop/src/screens/mod.rs` (`screens-chrome`): `pub fn root(ui: &mut Ui,
   state: &mut State, actions: &mut Vec<Action>)` splits the bands via
   [`Layout`] and calls `chrome::titlebar/sidebar/player_bar/queue_panel/toast/
   update_banner` + the active `main` view. Every screen function is
   `fn xxx(ui: &mut Ui, state: &mut State, actions: &mut Vec<Action>, rect: Rect)`
   (page views that manage their own header may stay `fn page(ui, state,
   actions) -> bool`). Screens never draw the titlebar/sidebar/player
   themselves, never call the backend, and never mutate `State` (except the
   documented local fields: `search.query`, `new_playlist`, `renaming`,
   `sidebar_width`, `window_maximized`). `app.rs` (Lead) only calls
   `screens::root` and applies returned actions.
8. **Decision log (Lead, 2026-10-18):** preview/`--screenshot` starts with
   `sidebar_width = 335` (interactive default 216); theme token corrections
   applied (`player_height 84`, `queue_height 235`, `queue_collapsed_height 45`,
   `sidebar_width_max 420`, plus scrollbar/cover/toast/banner/seek colors and
   `main_gutter/sidebar_item_height/sidebar_item_margin/search_*/seek_*/
   card_* /radius_sm2/radius_xl/radius_pill`); embedded Inter stays (glyph
   shapes differ slightly from the Segoe UI capture, layout matches via the
   §4.2 line boxes).
9. **Shared primitives** live in the `crates/rift-desktop/src/screens/cards/`
   directory module (split out of the former single-file `cards` module,
   1,725 lines; `mod.rs` re-exports everything so `screens::cards::*` call sites
   are unchanged) (`section_header`, `cover_tile`, `grid_metrics`, `card`,
   `track_row`, `slider`, `text_field`, generated texture ids
   `COVER_FALLBACK_TEX` / `LOGO_TEX`); other screen modules must reuse them
   rather than re-deriving the reference numbers.

---

## Appendix A — measured text ink positions (reference frame)

Useful for pixel-diff debugging. Ink bbox = first/last non-background pixel.

| Text | ink x | ink y |
|---|---|---|
| "Your library" | 362..450 | 71..87 |
| card name "Liked Songs" | 369..~447 | 283..295 |
| card sub "8 songs" | 369..~412 | 302..313 |
| "Recently played" | 362..~457 | 355..371 |
| recent title "Song Title 1" | 421..488 | 401..413 |
| recent artist "Artist Name" | 423..~490 | 415..426 (clipped in shot) |
| recent album "Album Name" | 653..721 | 409..417 |
| recent duration "3:07" | 1024..1045 | 409..417 |
| "UP NEXT (6)" | 371..443 | 441..449 |
| queue title 1 | 395..462 | 476..488 |
| queue artist 1 | 395..457 | 493..501 |
| seek time left "1:23" | 21..43 | 667..674 |
| seek time right "3:34" | 1119..1141 | 667..674 |
| np title "Song Title 1" | 70..~137 | 694..707 |
| np artist "Artist Name" | 70..~133 | 708..719 |
| Search placeholder | 353..496 | 17..26 |
| Search button label | 759..800 | 17..26 |
| RIFT wordmark | 45..88 | 18..29 |
| sidebar "Home" | 46..~78 | 71..84 (text) |
| sidebar "PLAYLISTS" | 20..80 | 159..165 |
