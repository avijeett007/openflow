/**
 * Flow OS increment 3 — validated categorical chart palette.
 *
 * FIXED order, CVD-safe on both dark and light backgrounds. DO NOT reorder or
 * cycle. Single-series charts (most of Mission Control) use `CHART_VIOLET`
 * only and carry NO legend — the chart title names the series. ≥2 series pull
 * colours from `CHART_PALETTE` in order and get a legend + direct labels.
 *
 * Status colours (running/success/warning/error/muted) are reserved and must
 * NEVER be reused as series colours — see the design contract §2.
 */
export const CHART_PALETTE = [
  "#7C5CFF", // violet
  "#D97706", // amber
  "#DB2777", // pink
  "#0891B2", // cyan
  "#059669", // green
] as const;

/** The single-series default (violet) — matches the brand accent. */
export const CHART_VIOLET = CHART_PALETTE[0];

/** Recessive grid hairline (~8% white on dark, derived from text token). */
export const CHART_GRID_COLOR =
  "color-mix(in srgb, var(--color-text) 10%, transparent)";

/** Axis tick / label ink — muted, never a series colour. */
export const CHART_AXIS_COLOR =
  "color-mix(in srgb, var(--color-text) 55%, transparent)";

/**
 * Shared recharts tooltip surface style: raised surface, hairline border,
 * 8px radius — matches the premium card tokens.
 */
export const CHART_TOOLTIP_STYLE: React.CSSProperties = {
  backgroundColor: "var(--color-of-raised)",
  border: "1px solid var(--color-of-hairline)",
  borderRadius: 8,
  fontSize: 12,
  boxShadow: "0 4px 16px rgba(0, 0, 0, 0.18)",
};

export const CHART_TOOLTIP_LABEL_STYLE: React.CSSProperties = {
  color: "var(--color-text)",
};
