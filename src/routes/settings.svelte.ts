/**
 * Application settings and library state management.
 *
 * Two singleton classes are exported:
 *  - `settings`        — user preferences (theme, font, UI visibility, etc.)
 *  - `libraryManager`  — active library connection and history
 *
 * Both are backed by `tauri-plugin-store` for persistence across restarts and
 * use Svelte 5 `$state` runes for reactivity. Import either singleton directly
 * into any component — no context or provider is required.
 *
 * @example
 * import { settings, libraryManager } from '$lib/settings.svelte';
 *
 * // Reactive read — updates any $derived or template that references it
 * settings.preferences.theme
 *
 * // Persistent write
 * await settings.set('theme', 'dark');
 */

import { LazyStore } from "@tauri-apps/plugin-store";
import { invoke } from "@tauri-apps/api/core";

// ── Persistence layer ─────────────────────────────────────────────────────────
//
// Single file, two logical keys: "preferences" and "library".
// Separate keys prevent a concurrent save from one class overwriting the other.
const store = new LazyStore("settings.json");

// ── Types ─────────────────────────────────────────────────────────────────────

export interface AppPreferences {
  theme: "light" | "dark" | "system";
  fontFamily: string;
  fontSize: "sm" | "md" | "lg";
  navItemVisibility: Record<string, boolean>;
  // Add new fields here. They will be filled from DEFAULT_PREFERENCES on first
  // load so existing installations are never broken by a new field.
}

interface LibraryState {
  activeLibrary: string | null;
  history: string[];
}

// ── Defaults ──────────────────────────────────────────────────────────────────

const DEFAULT_PREFERENCES: AppPreferences = {
  theme: "system",
  fontFamily: "Inter",
  fontSize: "md",
  navItemVisibility: {
    Uncategorized: true,
    Untagged: true,
    "Recently Used": true,
    "All Tags": true,
    Random: true,
    Trash: true,
  },
};

const DEFAULT_LIBRARY_STATE: LibraryState = {
  activeLibrary: null,
  history: [],
};

// ── SettingsStore ─────────────────────────────────────────────────────────────

class SettingsStore {
  preferences = $state<AppPreferences>({ ...DEFAULT_PREFERENCES });

  /**
   * Resolves when the initial load from disk is complete.
   *
   * Most UI can render with defaults while this is pending. Await it only in
   * components where rendering before load would cause a visible flash.
   */
  readonly ready: Promise<void>;

  constructor() {
    this.ready = this.load();

    // FUTURE: File watcher integration
    // If we want the app to react to external edits of settings.json
    // (e.g. for a "profiles / multiple config files" feature), set up a Tauri
    // event listener here that calls reloadFromDisk() when Rust emits a
    // "settings-file-changed" event via the `notify` crate.
    //
    // import { listen } from "@tauri-apps/api/event";
    // listen("settings-file-changed", () => this.reloadFromDisk());
  }

  private async load(): Promise<void> {
    const saved = await store.get<Partial<AppPreferences>>("preferences");
    if (!saved) return;

    // Deep merge: persisted values win, defaults fill any keys that didn't
    // exist when the user last saved (i.e. fields added in future updates).
    this.preferences = {
      ...DEFAULT_PREFERENCES,
      ...saved,
      navItemVisibility: {
        ...DEFAULT_PREFERENCES.navItemVisibility,
        ...(saved.navItemVisibility ?? {}),
      },
    };

    // ANTICIPATED: Push synced preferences to Rust after hydration
    // await this.syncBackendPreferences();
  }

  /**
   * Updates a single preference and immediately persists it to disk.
   * The Svelte reactive graph updates synchronously; the disk write is async.
   */
  async set<K extends keyof AppPreferences>(key: K, value: AppPreferences[K]): Promise<void> {
    await this.ready;
    this.preferences[key] = value;
    await this.persist();

    // ANTICIPATED: Backend sync
    // When a preference starts affecting Rust behavior, do two things:
    //
    //   1. Add the key to BACKEND_SYNCED_KEYS below.
    //   2. Add a match arm for it in the `apply_preference` Tauri command.
    //
    // if (BACKEND_SYNCED_KEYS.has(key)) {
    //   await invoke("apply_preference", { key, value });
    // }
    //
  }

  // ANTICIPATED: Backend-synced preference keys
  // Declare here which preference keys Rust needs to know about.
  //
  // const BACKEND_SYNCED_KEYS = new Set<keyof AppPreferences>([
  //   "maxImportSizeMb",
  //   "thumbnailQuality",
  // ]);
  //

  // ANTICIPATED: Push all synced preferences to Rust on startup
  // private async syncBackendPreferences(): Promise<void> {
  //   if (BACKEND_SYNCED_KEYS.size === 0) return;
  //   for (const key of BACKEND_SYNCED_KEYS) {
  //     await invoke("apply_preference", { key, value: this.preferences[key] });
  //   }
  // }
  //

  // FUTURE: Manual reload from disk
  // Uncomment and wire to a "Reload config" button in the settings UI.
  // Required before implementing the file watcher approach above.
  //
  // async reloadFromDisk(): Promise<void> {
  //   await store.load(); // forces the plugin to re-read the file from disk
  //   await this.load();  // re-hydrates this.preferences from the fresh data
  // }
  //

  private async persist(): Promise<void> {
    await store.set("preferences", $state.snapshot(this.preferences));
    await store.save();
  }
}

// ── LibraryManager ────────────────────────────────────────────────────────────

class LibraryManager {
  state = $state<LibraryState>({ ...DEFAULT_LIBRARY_STATE });

  /**
   * Set when startup reconnect fails (library moved or deleted while app was closed).
   *
   * Components should react to this with a $effect, show a toast, then clear it:
   *
   * @example
   * $effect(() => {
   *   if (libraryManager.connectionWarning) {
   *     toast.warning(libraryManager.connectionWarning);
   *     libraryManager.connectionWarning = null;
   *   }
   * });
   */
  connectionWarning = $state<string | null>(null);

  /** Resolves once stored library state has been loaded and connection attempted. */
  readonly ready: Promise<void>;

  constructor() {
    this.ready = this.load();
  }

  private async load(): Promise<void> {
    const saved = await store.get<LibraryState>("library");
    if (!saved) return;

    this.state = { ...DEFAULT_LIBRARY_STATE, ...saved };

    if (this.state.activeLibrary) {
      await this.reconnectOnStartup(this.state.activeLibrary);
    }
  }

  /**
   * Handles the startup reconnect attempt separately from manual switchLibrary.
   *
   * Contract:
   *  - Success → state unchanged, library is active and connected.
   *  - Any failure → activeLibrary is cleared, history is preserved,
   *    connectionWarning is set so the UI can show a toast.
   *
   * Why separate from switchLibrary: startup and manual reconnects have
   * different failure contracts. Manual failures throw for toast.promise()
   * to handle. Startup failures must never throw — they set reactive state
   * that the UI reads after mount.
   */
  private async reconnectOnStartup(path: string): Promise<void> {
    try {
      await invoke("connect_library", { libraryPath: path });
    } catch {
      // Any failure (deleted, moved, permissions) clears the active library.
      // The history entry is kept so the user can reconnect manually once
      // the issue is resolved.
      const libraryName = path.split("/").pop() ?? path;
      this.state.activeLibrary = null;
      this.connectionWarning = `Could not reconnect to "${libraryName}". The library may have been moved or deleted.`;
      await this.persist();
    }
  }

  /**
   * Connects to a library and makes it the active one.
   *
   * On success: updates active library, prepends to history (capped at 10 for now), persists.
   * On failure: re-throws so the calling site can handle it via toast.promise().
   */
  async switchLibrary(path: string): Promise<void> {
    await invoke("connect_library", { libraryPath: path });

    this.state.activeLibrary = path;
    this.state.history = [path, ...this.state.history.filter((p) => p !== path)].slice(0, 10);

    await this.persist();
  }

  /**
   * Removes a library from history and, if it was active, attempts a fallback
   * connection to the next entry. If no fallback is available, clears the
   * active library so the UI shows the "No library connected" Alert.
   */
  async removeFromHistory(path: string): Promise<void> {
    const wasActive = this.state.activeLibrary === path;
    this.state.history = this.state.history.filter((p) => p !== path);

    if (wasActive) {
      this.state.activeLibrary = null;

      if (this.state.history.length > 0) {
        // Best-effort fallback — failure is intentionally swallowed here.
        // The user ends up with no active library, which the UI handles via
        // the "No library connected" Alert.
        try {
          await this.switchLibrary(this.state.history[0]);
        } catch {
          this.state.activeLibrary = null;
        }
      }
    }

    await this.persist();
  }

  private async persist(): Promise<void> {
    await store.set("library", $state.snapshot(this.state));
    await store.save();
  }
}

// ── Singleton exports ─────────────────────────────────────────────────────────

export const settings = new SettingsStore();
export const libraryManager = new LibraryManager();
