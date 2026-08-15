# Tracked Visual Inventory

Source priority: supplied reference images, then PRD, then system design, then implementation prompt.

## Navigation
- Desktop uses a persistent left sidebar, about 240px wide, blacker than the page, with logo, primary nav, divider, active program summary, standing-list summary, and authenticated user card pinned low.
- Mobile uses a compact top header plus bottom navigation. It does not use the desktop sidebar or a dropdown menu.
- Approved destinations are Today, Program, Heatmap, Cohort, Reports, and Settings on desktop. Mobile bottom nav shows Today, Program, Heatmap, Cohort, and You.

## Page Shells
- Desktop content sits in a wide main canvas with a top header, a horizontal summary strip, then a task-first dashboard grid.
- Mobile screens are full-screen single-column routes with dense cards and touch-friendly rows.
- Cards are dark monochrome surfaces with thin borders, 8-12px radius, subtle hover/raised contrast, and very restrained shadowing.

## Typography
- Typeface is system sans. Headings are compact, bold, and not oversized.
- Labels are small uppercase or muted secondary text.
- Mobile header titles are centered where shown in the references; desktop greeting sits top-left.

## Spacing And Borders
- Desktop uses 16-24px page rhythm, thin separators, and compact inner card padding.
- Mobile uses 12-16px gutters and fixed bottom navigation spacing.
- Borders are low-opacity white; no bright colors, gradients, glassmorphism, or oversized shadows.

## Controls
- Buttons are compact rounded rectangles or icon-only circles.
- Checkboxes are square, bordered, and show a simple check when complete.
- Form controls match card styling: dark surface, thin border, 40px-ish control height.

## Charts And Heatmaps
- Heatmap cells are rounded rectangles/squares with grayscale intensity only.
- Cells must render from backend day data. Empty/missing history uses muted blank cells, not random decorative activity.
- Streak sparklines in references are secondary; if data is unavailable, show insufficient-data copy rather than fake charts.

## Task Rows
- Task list is the dominant panel on Today.
- Program and standing tasks stay visually separate.
- Rows include checkbox, task title, metadata when available, and a chevron/action affordance.

## Responsive Behavior
- 0-599px: mobile top header, bottom nav, one-column task-first content, no horizontal overflow.
- 600-767px: large mobile keeps bottom nav and slightly wider cards.
- 768-1023px: tablet may use rail/collapsed desktop structure and two columns where useful.
- 1024-1439px: persistent sidebar and dashboard grid.
- 1440px+: persistent sidebar, wide dashboard proportions, task panel remains dominant.
