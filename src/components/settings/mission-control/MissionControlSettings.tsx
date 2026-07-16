import React from "react";
import { MissionControlView } from "./MissionControlView";

/** Sidebar-section wrapper for Mission Control (matches the other sections'
 * `*Settings` export shape consumed by the section registry). */
export const MissionControlSettings: React.FC = () => <MissionControlView />;
