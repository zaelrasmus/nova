import { LazyStore } from "@tauri-apps/plugin-store";
import { invoke } from "@tauri-apps/api/core";

const store = new LazyStore("settings.json");

interface Settings {
  activeLibrary: string | null;
  history: string[];
}

class LibraryManager {
  state = $state<Settings>({
    activeLibrary: null,
    history: [],
  });

  constructor() {
    this.load();
  }

  private async load() {
    const saved = await store.get<Settings>("settings");
    if (saved) {
      this.state = saved;
      if (this.state.activeLibrary) {
        this.switchLibrary(this.state.activeLibrary);
      }
    }
  }

  private async save() {
    await store.set("settings", $state.snapshot(this.state));
    await store.save();
  }

  async switchLibrary(path: string) {
    try {
      await invoke("connect_library", { libraryPath: path });

      this.state.activeLibrary = path;

      const newHistory = [path, ...this.state.history.filter((p) => p !== path)];
      this.state.history = newHistory.slice(0, 10);

      await this.save();
    } catch (e: unknown) {
      // 'e' es unknown explícitamente
      // Estrechamos el tipo

      const errorMessage = e instanceof Error ? e.message : String(e);

      if (errorMessage.toString().includes("no such file")) {
        this.removeFromHistory(path);
      } else {
        console.error("No se pudo conectar a la librería:", errorMessage);
        throw new Error(errorMessage);
      }
    }
  }
  async removeFromHistory(path: string) {
    // 1. Generar el nuevo historial filtrado
    const updatedHistory = this.state.history.filter((p) => p !== path);
    const wasActive = this.state.activeLibrary === path;

    // 2. Actualizar el historial en el estado
    this.state.history = updatedHistory;

    // 3. Lógica de fallback si eliminamos la activa
    if (wasActive) {
      if (updatedHistory.length > 0) {
        // Intentar conectar a la más reciente que queda
        const nextLibrary = updatedHistory[0];
        try {
          await this.switchLibrary(nextLibrary);
          console.log(`Fallback: Reconectado a ${nextLibrary}`);
        } catch (err) {
          console.error("Error en el fallback automático:", err);
          this.state.activeLibrary = null;
        }
      } else {
        // No quedan librerías en el historial
        this.state.activeLibrary = null;
        // Opcional: Podrías emitir un comando a Rust para cerrar el pool actual
        // await invoke("close_current_connection");
      }
    }

    // 4. Persistir cambios
    await this.save();
  }
}

export const libraryManager = new LibraryManager();
