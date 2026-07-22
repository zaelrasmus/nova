import { LazyStore } from "@tauri-apps/plugin-store";
import { invoke } from "@tauri-apps/api/core";

const store = new LazyStore("settings.json");

export interface AppPreferences {
  theme: "light" | "dark" | "system";
  fontFamily: string;
  fontSize: "sm" | "md" | "lg";
  navItemVisibility: Record<string, boolean>;
  thumbnailQuality: "auto" | "lossy" | "lossless";
  /** WebP lossy quality (0-100). Applies to Lossy mode and Auto's lossy branch. */
  thumbnailLossyQuality: number;
  animateGifsInGrid: boolean;
  // Add new fields here. They will be filled from DEFAULT_PREFERENCES on first
  // load so existing installations are never broken by a new field.
}

interface LibraryState {
  activeLibrary: string | null;
  history: string[];
}

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
  thumbnailQuality: "auto",
  thumbnailLossyQuality: 82,
  animateGifsInGrid: false,
};

const DEFAULT_LIBRARY_STATE: LibraryState = {
  activeLibrary: null,
  history: [],
};

class SettingsStore {
  preferences = $state<AppPreferences>({ ...DEFAULT_PREFERENCES });

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

    // Restore history immediately, but keep activeLibrary null until the backend
    // pool is actually connected. Consumers treat activeLibrary as "library ready"
    // (AssetGrid fires assetLibrary.load() the moment it turns truthy), so setting
    // it before connect_library resolves races the manifest stream against an
    // unconnected pool — the first fetch fails with NoLibrary and, unlike the old
    // TanStack query, never retries.
    this.state = {
      ...DEFAULT_LIBRARY_STATE,
      history: saved.history ?? [],
      activeLibrary: null,
    };

    if (saved.activeLibrary) {
      try {
        await invoke("connect_library", { libraryPath: saved.activeLibrary });
        this.state.activeLibrary = saved.activeLibrary; // pool live → safe to load
      } catch {
        const name = saved.activeLibrary.split(/[\\/]/).pop() ?? saved.activeLibrary;
        this.connectionWarning = `Could not reconnect to "${name}". The library may have been moved or deleted.`;
        await this.persist();
      }
    }
  }

  async switchLibrary(path: string): Promise<void> {
    await invoke("connect_library", { libraryPath: path });

    this.state.activeLibrary = path;
    this.state.history = [path, ...this.state.history.filter((p) => p !== path)].slice(0, 10);

    await this.persist();
  }

  async removeFromHistory(path: string): Promise<void> {
    const wasActive = this.state.activeLibrary === path;
    this.state.history = this.state.history.filter((p) => p !== path);

    if (wasActive) {
      this.state.activeLibrary = null;

      if (this.state.history.length > 0) {
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

export const settings = new SettingsStore();
export const libraryManager = new LibraryManager();
