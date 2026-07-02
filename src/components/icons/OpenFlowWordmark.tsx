import React from "react";

// The OpenFlow text wordmark: violet "Open" + themed "Flow". Sized by `width`
// (approximate visual width in px) so it can drop in where the old SVG logo was.
const OpenFlowWordmark: React.FC<{ width?: number; className?: string }> = ({
  width = 200,
  className = "",
}) => (
  <div
    className={`flex items-baseline gap-1 font-extrabold tracking-tight leading-none select-none ${className}`}
    style={{ fontSize: Math.round(width / 4.2) }}
    aria-label="OpenFlow"
  >
    <span className="text-logo-primary">Open</span>
    <span className="text-text">Flow</span>
  </div>
);

export default OpenFlowWordmark;
