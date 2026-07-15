import React from "react";

interface CardProps extends React.HTMLAttributes<HTMLDivElement> {
  /** Use the slightly lighter "raised" surface (for nested / interactive cards). */
  raised?: boolean;
  /** Apply the subtle hover-rise affordance (for clickable cards). */
  interactive?: boolean;
  /** Padding preset. `none` lets the caller own all spacing. */
  padding?: "none" | "sm" | "md" | "lg";
}

const PADDING: Record<NonNullable<CardProps["padding"]>, string> = {
  none: "",
  sm: "p-3",
  md: "p-4",
  lg: "p-5",
};

/**
 * Shared premium surface card (Flow OS increment 3): consistent surface +
 * hairline border + 12px radius + padding. Adopted fully across Mission
 * Control; the design tokens live in `styles/theme.css`.
 */
export const Card = React.forwardRef<HTMLDivElement, CardProps>(
  (
    {
      raised = false,
      interactive = false,
      padding = "md",
      className = "",
      children,
      ...props
    },
    ref,
  ) => (
    <div
      ref={ref}
      className={`rounded-xl border border-of-hairline ${
        raised ? "bg-of-raised" : "bg-of-surface"
      } ${PADDING[padding]} ${
        interactive
          ? "of-hover-rise cursor-pointer hover:border-of-violet/40"
          : ""
      } ${className}`}
      {...props}
    >
      {children}
    </div>
  ),
);

Card.displayName = "Card";
