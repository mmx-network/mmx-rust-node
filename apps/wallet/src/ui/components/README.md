# UI kit

Everything screens need is exported from `src/ui/components` (components) and
`src/ui/shell` (app frame). Screens compose these components and the layout
primitives; screen-specific layout lives in one stylesheet per screen folder
(theme tokens only, classes prefixed with the screen name, e.g. `send-*`), which
styles may restyle like any component class. All user-facing strings passed in must be
short and task-focused (spec §6.5). Text from the network (memos, token symbols,
names) must be sanitized by `src/lib` before it reaches a component;
`Amount` and `AssetIcon` additionally clean symbols defensively.

```tsx
import { Page, PageHeader, Card, Stack, Button, Amount } from '../components';
```

Conventions

- `class` is accepted by every component and lands on its root element.
- Text inputs report values through `onValue(string)`; choices through `onChange(value)`.
- Icons are referenced by name (`IconName`, see `src/ui/icons.ts`).
- Refs: Preact function components do not forward `ref`; inputs take `inputRef` / `textareaRef`.
- Tones: `neutral | positive | negative | warning | accent` (`Tone`); banners use `info | positive | warning | danger`.
- Below 720 px is "mobile": dialogs become bottom sheets, grids collapse, touch targets are ≥ 44 px.

## Layout

| Component | Props | Notes |
|---|---|---|
| `Page` | `width?: 'narrow' (560) \| 'default' (960) \| 'wide'` | Screen container; 32 px rhythm between children. Class `page`, `page--*`. |
| `PageHeader` | `title`, `subtitle?`, `actions?`, `onBack?`, `backLabel? = 'Back'` | `h1`. Class `page-header`, `__title`, `__subtitle`, `__actions`, `__back`. |
| `Stack` | `gap? 0–12 (×4 px, default 4)`, `direction? 'column' \| 'row'`, `align?`, `justify? 'start'\|'center'\|'end'\|'between'`, `wrap?`, `responsive?` (row → column below 720), `as?`, `aria-*`, `data-*` | Flex box. `stack`, `stack--row`, `stack--gap-N`. |
| `Grid` | `columns? 1–4`, `min?` (e.g. `'14rem'`, auto-fit), `gap?` | Collapses to one column below 720 px. `grid`, `grid--cols-N`, `grid--auto`. |
| `Section` | `title`, `description?`, `actions?`, `level? 2\|3`, `card?` | Titled block; `card` wraps the body in a surface. `section`, `__header`, `__title`, `__actions`, `__body`, `section--card`. |
| `Card` | `variant? 'default'\|'sunken'\|'outline'\|'hero'`, `padding? 'none'\|'sm'\|'md'\|'lg'`, `as?` | `hero` is for the overview balance. `card`, `card--*`, `card--pad-*`. |

```tsx
<Page width="wide">
  <PageHeader title="Activity" actions={<Button icon="download">Export</Button>} />
  <Section title="Balance" card actions={<SegmentedControl … />}>…</Section>
</Page>
```

## Text

| Component | Props |
|---|---|
| `Text` | `as? 'p'\|'span'\|'div'\|'strong'\|'small'\|'time'`, `size? 'xs'\|'sm'\|'md'\|'lg'`, `tone? 'default'\|'muted'\|'faint'\|'accent'\|'positive'\|'negative'\|'warning'`, `weight?`, `mono?`, `numeric?` (tabular), `truncate?`, `breakAll?`, `align?` |
| `Heading` | `level 1–4`, `size? 'xl'\|'lg'\|'md'\|'sm'` |
| `VisuallyHidden` | screen-reader-only text |

## Actions

| Component | Props | Notes |
|---|---|---|
| `Button` | `variant? 'primary'\|'secondary'(default)\|'ghost'\|'danger'`, `size? 'sm'\|'md'\|'lg'`, `icon?`, `iconEnd?`, `loading?`, `block?`, `type? = 'button'`, native button attributes | `loading` keeps focus, shows a spinner and swallows clicks and implicit form submits (no double submission). Classes `btn`, `btn--{variant}`, `btn--{size}`, `btn--block`, `btn--loading`, `btn__icon`, `btn__label`. |
| `IconButton` | `icon`, `label` (required: aria-label + title), `variant? 'ghost'\|'secondary'\|'primary'\|'danger'`, `size?`, `loading?`, `noTitle?` | `icon-btn`, `icon-btn--*`. |
| `CopyButton` | `onCopy()` (e.g. `() => backend.copyText(text)`), `label? = 'Copy'`, `copiedLabel? = 'Copied'`, `variant? 'icon'\|'button'`, `buttonVariant?`, `size?` | Shows a check for 1.6 s and announces "Copied". A rejected promise shows nothing. `copy-btn`, `copy-btn--copied`. |

## Forms

Wrap every control in `Field`; it wires the label, hint and error to the control
(id, `aria-describedby`, `aria-invalid`) through context.

| Component | Props | Notes |
|---|---|---|
| `Field` | `label`, `hint?`, `error?`, `counter?`, `labelAction?`, `optional?`, `hideLabel?`, `id?` | Error replaces the hint and is announced politely. `field`, `field--invalid`, `__label`, `__hint`, `__error`, `__counter`. |
| `TextInput` | `value`, `onValue`, `leading?`, `trailing?`, `mono?`, `sensitive?`, `invalid?`, `size? 'md'\|'lg'`, `inputRef?`, native input attributes | `sensitive` disables spellcheck/autocorrect/autocapitalize/translation (recovery words, codes). Classes `input`, `input--mono`, `input--invalid`, `input__control`, `input__adornment`. |
| `PasswordInput` | as TextInput + `revealable? = true` | Reveal toggle; passes `name`, `autocomplete`, `data-*` (e.g. `data-1p-ignore`, `data-lpignore`, `data-bwignore`) to the `<input>`; switches back to `type="password"` when its form submits. Always `sensitive`. |
| `Select` | `value`, `onValue`, `options: {value,label,disabled?}[]`, `size? 'sm'\|'md'` | Native select. `select`, `select__control`. |
| `SearchInput` | `value`, `onValue`, `label` (aria), `placeholder?` | Clear button; Esc clears. |
| `Combobox` | TextInput props + `options: {id,label,description?,leading?,trailing?}[]`, `onSelect(option)`, `listLabel`, `autoHighlight?` | Input with a suggestion list (ARIA combobox): the caller filters `options` from `value`; ↑ ↓ Enter Esc. Use for the send recipient (contacts) and recovery-word entry (`autoHighlight` + `sensitive`; handle paste with `onPaste`). `combobox`, `combobox__list`, `combobox__option--active`. |
| `NumberStepper` | `value`, `onChange(n)`, `min`, `max`, `step?`, `label?` | Integers only; clamps on blur/Enter; ↑ ↓ PgUp PgDn Home End. `stepper`. |
| `SegmentedControl<T>` | `value`, `onChange`, `options: {value,label,icon?,iconOnly?,disabled?}[]`, `label?`, `size?`, `block?` | Radio semantics, arrow keys. Use for filters and chart ranges. `segmented`, `segmented__item--active`. |
| `Switch` | `checked`, `onChange`, `label?`, `aria-label?` | `role="switch"`. `switch`, `switch--on`. |
| `OptionRow` | `title`, `description?`, `children` (the control), `stacked?`, `tone? 'default'\|'danger'` | Settings row: text on the start side, control on the end side; rows inside a `Card` get dividers. `option-row`, `option-row__title`, `option-row__control`. |

```tsx
<Field label="Amount" error={amountError} labelAction={<Text as="span" size="sm" tone="muted">Available {available}</Text>}>
  <TextInput value={amount} onValue={setAmount} inputMode="decimal"
    trailing={<Button size="sm" variant="ghost" onClick={useMax}>Max</Button>} />
</Field>
<Field label="Memo" optional counter={bytes > 48 ? `${bytes}/64` : undefined}>
  <TextInput value={memo} onValue={setMemo} />
</Field>
<form onSubmit={save}>
  <PasswordInput name="password" autocomplete="new-password" value={words} onValue={setWords} />
</form>
```

## Data display

| Component | Props | Notes |
|---|---|---|
| `Amount` | `value: bigint` (atomic), `decimals`, `symbol?`, `sign? 'auto'\|'always'\|'never'`, `size? 'sm'\|'md'\|'lg'\|'xl'\|'display'`, `tone? 'auto'\|'neutral'\|'muted'\|'positive'\|'negative'`, `locale?` | Formatted by `src/lib/amount.ts` in the app's locale: exact (never floating point), trailing zeros trimmed; fraction de-emphasised; `−` is U+2212. `tone='auto'` colours signed positives. `amount`, `amount__int`, `amount__frac`, `amount__symbol`, `amount--{size}`, `amount--{tone}`. |
| `AddressText` | `address`, `mode? 'full'\|'short'`, `chunked? = true`, `onCopy?`, `copyLabel?` | Full mode groups 4 characters (alternate groups tinted) for checking; short = `mmx1abcdef…uvwxyz` with the full address as title. Helpers `shortenAddress`, `chunkAddress`. `address`, `address__chunk`. |
| `Identicon` | `value`, `size? = 32`, `label?` | Deterministic avatar (FNV-1a + xorshift). Seed wallets with their fingerprint, contacts/addresses with the address. `identicon` (radius via `--identicon-radius`). |
| `AssetIcon` | `contract`, `symbol?`, `native?`, `size?` | MMX mark for the native currency (decide by the zero contract), identicon + initial for tokens. |
| `IconTile` | `icon`, `tone?`, `size? 'sm'\|'md'\|'lg'`, `label?` | Activity direction markers: received `arrow-down-left`/positive, sent `arrow-up-right`, reward `reward`/accent, own transfer `swap`, pending `clock`/warning, failed `x-circle`/negative. |
| `Badge` | `tone?`, `icon?`, `dot?` | `badge`, `badge--{tone}`. |
| `KeyValueList` / `KeyValue` | list: `variant? 'rows'\|'compact'`; row: `label`, `hint?`, `stacked?`, `emphasis?` | Review and detail screens (`<dl>`). Use `stacked` for full addresses, `emphasis` for the total. `kv`, `kv__row`, `kv__label`, `kv__value`. |
| `List` / `ListItem` / `ListGroup` | List: `label?`, `inset?`, `divided?`. Item: `leading?`, `title`, `subtitle?`, `trailing?`, `trailingSub?`, `onClick?`, `actions?`, `chevron?`, `selected?`, `disabled?`. Group: `title`, `aside?`, `inset?` | `onClick` makes the row a button; `actions` render outside it (no nested buttons). `ListGroup` for date groups. `list`, `list--inset`, `list-item`, `list-item__*`, `list-group`. |
| `QRCode` | `value`, `label` (aria), `size? = 200`, `ecc? = 'M'` | Always dark on white with a 4-module quiet zone. `qr`. |
| `WordGrid` | `words`, `hidden?`, `onReveal?`, `revealLabel?`, `start?` | Numbered recovery phrase. While `hidden` the words are not in the DOM. Text selection is disabled; offer a CopyButton with `sensitive` copy instead. `word-grid`. |
| `BrandMark` / `BrandLockup` | mark: `size?`, `variant? 'color'\|'glyph'`, `label?`; lockup: `size?`, `name? = 'MMX Wallet'` | Official MMX mark. Disc colour via `--brand-disc`. |

## Feedback

| Component | Props | Notes |
|---|---|---|
| `Banner` | `tone? 'info'\|'positive'\|'warning'\|'danger'`, `title?`, `icon?`, `action?`, `onDismiss?` | Inline status; `danger` uses `role="alert"`. `banner`, `banner--{tone}`. |
| `EmptyState` | `icon`, `title`, `message?` (one line), `action?`, `size? 'sm'\|'md'`, `tone? 'accent'\|'positive'\|'warning'\|'negative'\|'neutral'` | Also the result screen of a completed action (`tone="positive"`, `icon="check-circle"`). `empty-state`, `empty-state--{tone}`. |
| `Disclosure` | `title`, `meta?`, `icon?`, `defaultOpen?` or `open` + `onToggle` | Collapsible group, e.g. "Other tokens" (`icon="warning"`, `meta` = count). `disclosure`, `disclosure--open`. |
| `Spinner` | `size?`, `label?` | Decorative unless `label` is given. |
| `Skeleton` | `width?`, `height?`, `lines?`, `shape? 'text'\|'rect'\|'circle'` | Put `aria-busy="true"` on the loading container. |
| `ProgressSteps` | `steps: string[]`, `current` (0-based), `label?` | "Step 2 of 4 · Name" over a segmented bar. `steps`, `steps__item--done/current/upcoming`. |
| `ToastViewport` | `toasts: Toast[]`, `onDismiss(id)` | Render once at the app root, after `<Shell>` (on mobile it then sits above the tab bar). `Toast = {id, tone?, title, message?, action?: {label,onClick}, timeout? (ms, default 5000, 0 = sticky)}`. The viewport runs the timers (paused on hover/focus) and calls `onDismiss`; the store only adds/removes. The viewport itself is the live region (errors assertive, others polite) and adds a new toast one frame after showing itself on top, so screen readers announce it; toasts carry no role of their own. |

## Overlays

| Component | Props | Notes |
|---|---|---|
| `Dialog` | `open`, `onClose`, `title`, `description?`, `footer?`, `size? 'sm'\|'md'\|'lg'`, `dismissible? = true`, `initialFocus?`, `hideTitle?` | Native modal `<dialog>` in the top layer: focus trap, Esc/backdrop close (when dismissible), focus returns to the opener, page scroll locked. Initial focus: `initialFocus`, else the first element with `data-autofocus`, else the panel. Footer: secondary first, primary last. Bottom sheet below 720 px. `dialog`, `dialog__panel`, `__header`, `__title`, `__body`, `__footer`. |
| `Sheet` | same as Dialog | Side panel (desktop) / bottom sheet (mobile) for details such as an activity item. `dialog--sheet`. |
| `ConfirmDialog` | `open`, `title`, `message?`, `confirmLabel`, `cancelLabel?`, `tone? 'default'\|'danger'`, `busy?`, `confirmDisabled?`, `onConfirm`, `onCancel`, `children?` | `busy` shows a spinner and blocks closing. |
| `Menu` | `label`, `items: (MenuItem \| {separator: true})[]`, `trigger(props, open)`, `placement?`, `matchWidth?` | Spread `props` onto the trigger (`<IconButton {...props} icon="more" label="More" />`). `MenuItem = {id?, label, description?, icon?, leading?, trailing?, checked?, tone?, disabled?, textValue?, onSelect}`; `checked` turns items into radio items. ↑ ↓ Home End, type-ahead, Esc, Tab, outside click. `menu`, `menu__item`. |
| `Tooltip` | `content`, `children` (one focusable element), `placement?`, `describe? = true`, `disabled?`, `delay?` | Hover (after delay) and keyboard focus; Esc hides. `describe` adds `aria-describedby`; use `false` when the element's name already says it. `tooltip`. |

Floating panels use `position: fixed` and the Popover API top layer when
available, so they are never clipped by scroll containers.

## Shell (`src/ui/shell`)

- `Shell` — `{ route, onNavigate, wallets, activeWalletId?, onSelectWallet, onAddWallet, network: {state, height?, network?}, canLock, onLock, children }`.
  Brand, wallet switcher (menu: wallets with ID, lock state, "Add wallet"; a wallet that
  can't be opened is listed but disabled), navigation,
  network status (tooltip shows the block height), lock. Layout from `data-layout`:
  `sidebar` (collapses to icons 720–1024 px), `rail`, `topbar`; below 720 px a top bar
  plus bottom tabs (Overview, Send, Receive, Activity, More → Contacts, Settings).
  Includes a "Skip to content" button. On route changes it sets the document title
  ("Send · MMX Wallet"), scrolls to the top and, when the focused control went away with
  the old page, focuses the new page's `h1` (or `<main>`). A page that focuses a field on
  mount keeps that focus.
- `FocusLayout` — `{ onBack?, backLabel?, headerAction?, width? 'narrow'|'wide', footer?, children }`:
  full-page layout without navigation for welcome, unlock, create and import flows.
- `NetworkStatus`, `WalletSwitcher`, `networkName('testnet12') → 'Testnet 12'`, `networkLabel`.

## Theming contract

`src/styles/index.ts` loads `base.css` and the active theme, sets
`data-style` and `data-layout` on `<html>`, and exports:

- `applyAppearance('system' | 'light' | 'dark')` — sets `data-scheme` (following the OS live for `system`) and `<meta name="theme-color">`.
- `stylesReady: Promise<void>` — await before the first render to avoid a flash of the fallback theme.
- `activeTheme: ThemeMeta`.

Selection: `VITE_WALLET_STYLE=<id>` at build time (default `obsidian`); dev builds also
accept `?style=<id>` and `?layout=<sidebar|rail|topbar>`.

Adding a style: create `src/styles/themes/<id>/theme.css` and `theme.ts`
(`ThemeMeta = { id, name, layout, fonts, themeColor: {light, dark} }`), then add one
line to `src/styles/themes/index.ts`. Fonts are bundled with the theme
(`@fontsource/*` imports or local `@font-face`), never remote.

A theme declares the tokens for both schemes on `[data-scheme='light']` (plus
`:root`) and `[data-scheme='dark']` — attribute selectors, not only `:root`, so
scoped previews work. It may restyle any class listed above; it must not rely on
markup changes.

Contract tokens (spec §8): `--font-ui --font-display --font-mono --bg --bg-elevated
--bg-sunken --surface --surface-2 --border --border-strong --text --text-muted
--text-faint --accent --accent-contrast --accent-soft --positive --negative
--warning --focus --radius-s/m/l --shadow-1/2 --chart-line --chart-fill --chart-grid
--space-unit --nav-width --layout`.

Derived tokens (defaults in `base.css`, override as needed): `--icon-stroke`,
type scale `--text-xs … --text-display`, `--weight-*`, `--tracking-display`,
spacing `--s1 … --s12` (from `--space-unit`), `--control-h-sm/--control-h/--control-h-lg`,
`--control-radius`, `--control-bg`, `--control-border`, `--card-radius`, `--card-bg`,
`--card-border`, `--card-shadow`, `--state-hover`, `--state-press`, `--accent-hover`,
`--accent-press`, `--positive-soft`, `--negative-soft`, `--warning-soft`, `--info-soft`,
`--on-negative`, `--backdrop`, `--ring`, `--nav-bg`, `--nav-border`, `--nav-item-*`,
`--rail-width`, `--topbar-height`, `--mobilebar-height`, `--tabbar-height`,
`--content-max`, `--qr-frame`, `--identicon-radius`, `--brand-disc`, `--brand-glyph`,
`--duration-fast`, `--duration`, `--ease`.

Requirements for every theme: WCAG AA contrast (text 4.5:1, large text and control
boundaries 3:1) in both schemes, visible `:focus-visible` rings, QR codes dark on
light, tabular numerals for amounts in lists, `prefers-reduced-motion` respected
(handled in `base.css`).

Shell classes: `shell`, `shell__nav`, `shell__brand`, `shell__switcher`,
`shell__navlist`, `nav-item`, `nav-item--active`, `nav-item__icon`, `nav-item__label`,
`shell__status`, `shell__lock`, `shell__main`, `shell__content`, `shell__mobilebar`,
`tabbar`, `tabbar__item`, `tabbar__item--active`, `switcher`, `switcher__avatar`,
`switcher__name`, `switcher__meta`, `net-status`, `net-status--{state}`,
`net-status__dot`, `focus-layout`, `focus-layout__header`, `focus-layout__content`.

The development gallery (`kitchen.html`, `src/ui/dev`) renders every component in
both schemes: `npx vite --port 5174` then open `/kitchen.html`
(`?view=shell|focus`, `?single=light|dark`, `?route=`, `?net=`, `?layout=`, `?style=`).
It is not part of the production build.
