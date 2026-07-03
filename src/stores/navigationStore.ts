import { create } from "zustand";
import type { SidebarSection } from "../components/Sidebar";

// The active settings section is the single navigation state for the main app
// window (the sidebar and content area both read it). Kept in a tiny store — as
// opposed to App-local state — so components deep in the tree (e.g. the
// hands-free "Go to Models" shortcut) can navigate without prop-drilling.
interface NavigationStore {
  currentSection: SidebarSection;
  setCurrentSection: (section: SidebarSection) => void;
}

export const useNavigationStore = create<NavigationStore>((set) => ({
  currentSection: "general",
  setCurrentSection: (section) => set({ currentSection: section }),
}));
