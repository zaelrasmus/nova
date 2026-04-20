/**
 * Settings section registry.
 *
 * To add a new settings category:
 *  1. Add a new entry to SETTINGS_SECTIONS below.
 *  2. Create the corresponding content component.
 *  3. Register it in the `sectionComponents` map in SettingsDialog.svelte.
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

export const SETTINGS_SECTIONS: SettingsSection[] = [
  {
    id: "appearance",
    label: "Appearance",
    icon: "Palette",
  },
  {
    id: "import",
    label: "Import",
    icon: "FolderInput",
  },
  {
    id: "display",
    label: "Display",
    icon: "LayoutGrid",
  },
  {
    id: "library",
    label: "Library",
    icon: "Library",
  },
  {
    id: "about",
    label: "About",
    icon: "Info",
    dividerAbove: true,
  },
];

export const DEFAULT_SECTION_ID = SETTINGS_SECTIONS[0].id;
