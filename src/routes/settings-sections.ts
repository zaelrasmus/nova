/**
 * Settings section registry.
 *
 * To add a new settings category:
 *  1. Add a new entry to SETTINGS_SECTIONS below.
 *  2. Create the corresponding content component.
 *  3. Register it in the `sectionComponents` map in +page.svelte.
 *
 * That's it — the sidebar, routing, and active state are all derived
 * from this array automatically.
 */

import type { Component } from "svelte";

export interface SettingsSection {
  /** Unique identifier used for active state tracking. */
  id: string;
  /** Label shown in the sidebar. */
  label: string;
  /** Lucide icon name — must be imported in SettingsDialog.svelte. */
  icon: string;
  /** Optional divider rendered above this section in the sidebar. */
  dividerAbove?: boolean;
}

/**
 * EVERY entry here must have a component registered in `sectionComponents`
 * (+page.svelte). A section listed without one renders an empty panel — which is
 * what "Library" and "About" did, and what "Appearance" effectively did with its
 * controls commented out. A tab that opens onto nothing reads as a broken app,
 * so a section earns its place by having something to show.
 */
export const SETTINGS_SECTIONS: SettingsSection[] = [
  {
    id: "display",
    label: "Display",
    icon: "LayoutGrid",
  },
  {
    id: "extension",
    label: "Browser extension",
    icon: "Plug",
  },
  {
    id: "about",
    label: "About",
    icon: "Info",
    dividerAbove: true,
  },
];

export const DEFAULT_SECTION_ID = SETTINGS_SECTIONS[0].id;
