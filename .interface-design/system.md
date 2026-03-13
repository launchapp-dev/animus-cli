# AO Web UI — Design System

Extracted from `crates/orchestrator-web-server/web-ui/src/`
Direction: **Precision & Density** — dark, data-focused orchestrator dashboard

---

## Theme

- **Mode**: Dark only (oklch color space)
- **Background**: `oklch(0.11 0.01 260)` with subtle radial gradients (primary at 4%, secondary at 3%)
- **Primary accent**: Teal-cyan `oklch(0.78 0.15 175)` — used for active states, links, glow effects
- **Hue axis**: 260 (blue-slate) for neutrals, 175 (teal) for primary

## Fonts

- **Sans**: `Sora Variable` (display + body), fallback: system-ui, sans-serif
- **Mono**: `JetBrains Mono` (IDs, code, metrics), weights: 400, 500, 600
- **Feature settings**: `"cv02", "cv03", "cv04"` on html

## Color Tokens

### Semantic (use these, not raw values)

| Token | Value | Use |
|-------|-------|-----|
| `--background` | `oklch(0.11 0.01 260)` | Page background |
| `--foreground` | `oklch(0.93 0.01 260)` | Primary text |
| `--card` | `oklch(0.14 0.01 260)` | Card surfaces |
| `--primary` | `oklch(0.78 0.15 175)` | Accent, active, links |
| `--primary-foreground` | `oklch(0.13 0.02 175)` | Text on primary |
| `--secondary` | `oklch(0.18 0.01 260)` | Secondary surfaces |
| `--muted` | `oklch(0.18 0.01 260)` | Muted backgrounds |
| `--muted-foreground` | `oklch(0.55 0.02 260)` | Secondary text |
| `--accent` | `oklch(0.20 0.01 260)` | Hover surfaces |
| `--destructive` | `oklch(0.65 0.22 25)` | Error, danger |
| `--border` | `oklch(1 0 0 / 8%)` | Default borders |
| `--input` | `oklch(1 0 0 / 10%)` | Input borders |

### Status Colors (custom)

| Token | Value | Use |
|-------|-------|-----|
| `--ao-success` | `oklch(0.72 0.19 155)` | Done, completed, healthy |
| `--ao-running` | `oklch(0.70 0.15 250)` | In-progress, running |
| `--ao-amber` | `oklch(0.80 0.16 75)` | Warning, paused |
| `--ao-glow` | `oklch(0.78 0.15 175 / 15%)` | Glow effects |
| `--ao-surface` | `oklch(0.15 0.012 260)` | Sidebar, elevated surfaces |
| `--ao-surface-hover` | `oklch(0.18 0.015 260)` | Surface hover state |

### Chart Colors

| Token | Hue | Use |
|-------|-----|-----|
| `--chart-1` | 175 (teal) | Primary metric |
| `--chart-2` | 250 (blue) | Secondary metric |
| `--chart-3` | 60 (amber) | Tertiary metric |
| `--chart-4` | 25 (red) | Alert metric |
| `--chart-5` | 310 (purple) | Accent metric |

### Rule: No hardcoded Tailwind colors

Never use `green-500`, `blue-500`, `red-500`, etc. Map to semantic tokens:
- Success/completed → `--ao-success` or `text-primary` (for checkmarks)
- Running/in-progress → `--ao-running`
- Failed/error → `text-destructive`
- Warning/paused → `--ao-amber`

## Spacing

**Base unit**: 4px (Tailwind default)
**Scale** (by frequency in codebase):

| Tailwind | px | Primary use |
|----------|-----|-------------|
| `0.5` | 2px | Micro gaps (status dot + label) |
| `1` | 4px | Inline element gaps, list items |
| `1.5` | 6px | Tight flex gaps |
| `2` | 8px | Card content spacing, small gaps |
| `3` | 12px | Form field spacing, stat card gaps |
| `4` | 16px | Page section gaps, grid gaps, card padding |
| `5` | 20px | Main content padding (mobile) |
| `6` | 24px | Page-level section spacing, main content padding (desktop) |

### Page Layout Rules

- Page root: `space-y-4` (list pages) or `space-y-6` (detail pages with sections)
- Grid gaps: `gap-2` (stat cards, filters), `gap-3` (stat grids), `gap-4` (card grids)
- Card internal: `space-y-2` (tight content) or `space-y-3` (forms)
- Main content area: `p-5 md:p-6`

## Typography Scale

| Role | Classes | Use |
|------|---------|-----|
| Page title | `text-xl font-semibold tracking-tight` | Dashboard (dense) |
| Page title (standard) | `text-2xl font-semibold tracking-tight` | All other pages |
| Section heading (uppercase) | `text-xs uppercase tracking-wider text-muted-foreground/60 font-medium` | Dashboard card headers |
| Section heading (standard) | `text-sm font-medium` | Card titles across pages |
| Body | `text-sm` | Default content text |
| Caption / meta | `text-xs text-muted-foreground` | Timestamps, secondary info |
| Micro label | `text-[10px]` or `text-[11px]` | Badge text, sidebar subtitle |
| Monospace ID | `font-mono text-xs` | Task IDs, run IDs, phase IDs |
| Stat value | `text-xl font-semibold font-mono` | Metric numbers |
| Stat label | `text-[11px] text-muted-foreground/70 uppercase tracking-wider font-medium` | Above metric numbers |

### Rule: Standardize page titles

Use `text-xl` only for the Dashboard (denser layout). All other pages: `text-2xl`.

## Radius

**Base**: `0.5rem` (8px), defined as `--radius`

| Token | Value | Use |
|-------|-------|-----|
| `rounded-sm` | `calc(0.5rem * 0.6)` = ~4.8px | Small pills |
| `rounded-md` | `calc(0.5rem * 0.8)` = ~6.4px | Buttons, inputs, badges (shadcn default) |
| `rounded-lg` | `0.5rem` = 8px | Cards, dialogs |
| `rounded-full` | 999px | Badges, status dots |

## Depth

**Strategy**: Borders-only with selective glow

- **Default**: `border border-border/40` (subtle, low contrast)
- **Cards**: `border-border/40 bg-card/60` with optional `backdrop-blur-sm`
- **Elevated surfaces**: `bg-[var(--ao-surface)]` (sidebar, header)
- **Glow accent**: `.ao-glow-border` — `box-shadow: 0 0 0 1px primary/15%, 0 0 20px -5px primary/10%`
- **No drop shadows** on cards or panels (dark theme makes them invisible)
- **Exception**: Command palette uses `shadow-2xl shadow-black/40` (modal overlay)

### Rule: No box-shadow on cards

Use borders and background opacity for depth hierarchy:
- Level 0: `bg-background` (page)
- Level 1: `bg-card/60 border-border/40` (cards)
- Level 2: `bg-[var(--ao-surface)]` (sidebar, header)
- Level 3: Modal backdrop + `shadow-2xl` (dialogs only)

## Component Patterns

### Card (data section)

```
<Card className="border-border/40 bg-card/60">
  <CardHeader className="pb-2 pt-3 px-4">
    <CardTitle className="text-xs uppercase tracking-wider text-muted-foreground/60 font-medium">
      {title}
    </CardTitle>
  </CardHeader>
  <CardContent className="px-4 pb-3 space-y-2">
    {children}
  </CardContent>
</Card>
```

Use this pattern for dashboard-style info cards. For form/action cards, omit the uppercase header and use `text-sm font-medium` instead.

### Card (form/action section)

```
<Card>
  <CardHeader className="pb-2">
    <CardTitle className="text-sm font-medium">{title}</CardTitle>
  </CardHeader>
  <CardContent className="space-y-3">
    {children}
  </CardContent>
</Card>
```

### StatCard

```
<Card className="border-border/40 bg-card/60 backdrop-blur-sm transition-colors hover:border-border/60 {accent ? 'ao-glow-border' : ''}">
  <CardContent className="pt-3 pb-3 px-4">
    <p className="text-[11px] text-muted-foreground/70 uppercase tracking-wider font-medium">{label}</p>
    <p className="text-xl font-semibold font-mono mt-0.5 {accent ? 'text-primary' : 'text-foreground/90'}">{value}</p>
  </CardContent>
</Card>
```

### StatusDot

8x8px circle, flex-shrink-0. States:
- `--live` (success): `bg-[--ao-success]` + 6px glow + pulse animation
- `--running`: `bg-[--ao-running]` + 6px glow + pulse animation
- `--error`: `bg-destructive` + 6px glow, no animation
- `--idle`: `bg-muted-foreground`

### Badge

Uses shadcn Badge with variant mapping:
- `statusColor()`: done/completed → `default`, in-progress → `secondary`, blocked/failed → `destructive`, else → `outline`
- `priorityColor()`: critical → `destructive`, high → `secondary`, else → `outline`

### Empty State

```
<p className="text-sm text-muted-foreground py-8 text-center">
  {message}
</p>
```

### Feedback Alert

```
<Alert variant={kind === "error" ? "destructive" : "default"}>
  <AlertDescription>{message}</AlertDescription>
</Alert>
```

### Native Select (form context)

```
<select className="h-9 w-full rounded-md border border-input bg-background px-3 text-sm">
```

Note: shadcn `Select` component exists but is unused. Consider migrating or standardizing.

### Compact Input (inline forms)

```
<Input className="h-8 text-xs" />
```

### Inline Action Buttons

```
<Button size="sm" variant="outline" className="h-6 text-xs">Action</Button>
```

## Animation

- **Fade in**: `.ao-fade-in` — `opacity: 0 → 1` over 300ms ease-out
- **Slide up**: `.ao-slide-up` — `opacity: 0, translateY(8px) → visible` over 350ms
- **Pulse**: `.ao-status-dot--live` — `opacity: 1 → 0.5 → 1` over 2s infinite
- **Reduced motion**: All animations collapse to 0.01ms
- **Transitions**: 150ms for color/opacity changes (`transition-all duration-150` on nav items)

## Layout

### App Shell

- Sidebar: `w-60`, hidden on mobile, sheet drawer on `md:` breakpoint
- Header: `h-11`, sticky, backdrop-blur, `bg-[var(--ao-surface)]/60`
- Main content: `flex-1 overflow-y-auto p-5 md:p-6`, max-width `max-w-6xl`
- Sidebar dividers: `h-px bg-border/50 mx-3`

### Navigation

- Nav items: `text-[13px]`, `rounded-md px-2.5 py-1.5`
- Active indicator: 2px wide, 16px tall, `bg-primary`, left-aligned pill
- Active state: `text-primary font-medium bg-primary/8`
- Inactive state: `text-muted-foreground hover:text-foreground/80 hover:bg-accent/40`
- Icon size: `h-3.5 w-3.5`

### Grid Patterns

| Context | Pattern |
|---------|---------|
| Stat row | `grid grid-cols-2 md:grid-cols-4 gap-3` |
| Card grid | `grid md:grid-cols-2 gap-4` |
| Filter chips | `grid grid-cols-3 md:grid-cols-6 gap-2` |
| Form fields | `grid grid-cols-2 gap-4` or `grid grid-cols-2 md:grid-cols-4 gap-3` |

## Scrollbar

Custom webkit scrollbar:
- Width/height: 6px
- Track: transparent
- Thumb: `oklch(1 0 0 / 12%)`, 3px radius
- Thumb hover: `oklch(1 0 0 / 20%)`

## Resolved Violations

1. ~~`styles.css`~~: Deleted — was dead code, no imports referenced it.
2. ~~Hardcoded colors in `workflow-pages.tsx`~~: Phase pills now use `Badge` + `statusColor()`. Timeline dots use `--ao-success`, `--ao-running`, `bg-destructive`.
3. ~~Checklist in `tasks-pages.tsx`~~: Now uses `text-[var(--ao-success)]`.
4. **Dashboard title**: Uses `text-xl` — intentional exception (denser layout).
5. ~~Planning title style~~: `font-bold` → `font-semibold tracking-tight` across all 4 planning pages.

## Open Items

- **Native selects**: Used in ~6 places instead of shadcn Select. Low priority — shadcn Select uses `@base-ui/react` with a different API, migration is non-trivial.
