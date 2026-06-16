import {
  App,
  Plugin,
  PluginSettingTab,
  Setting,
  Notice,
  TFile,
  normalizePath,
  Modal,
} from "obsidian";
import { execSync, spawn } from "child_process";
import * as path from "path";
import * as fs from "fs";

// ─── Interfaces ────────────────────────────────────────────────────────

interface MimirPluginSettings {
  /** Path to the mimir binary */
  mimirBinaryPath: string;
  /** Path to the Mimir SQLite database */
  mimirDbPath: string;
  /** Sync folder name within the vault (relative) */
  syncFolder: string;
  /** Auto-sync interval in minutes (0 = disabled) */
  autoSyncIntervalMinutes: number;
  /** Show sync status in the status bar */
  showStatusBar: boolean;
}

const DEFAULT_SETTINGS: MimirPluginSettings = {
  mimirBinaryPath: "mimir",
  mimirDbPath: "",
  syncFolder: "mimir",
  autoSyncIntervalMinutes: 0,
  showStatusBar: true,
};

// ─── Plugin ────────────────────────────────────────────────────────────

export default class MimirVaultSyncPlugin extends Plugin {
  settings: MimirPluginSettings;
  statusBar: HTMLElement | null = null;
  syncInterval: number | null = null;

  async onload() {
    await this.loadSettings();

    // Ensure sync folder exists
    const syncDir = this.getSyncFolderPath();
    if (!fs.existsSync(syncDir)) {
      fs.mkdirSync(syncDir, { recursive: true });
    }

    // Commands
    this.addCommand({
      id: "mimir-sync-now",
      name: "Sync now (pull from Mimir)",
      callback: () => this.pullFromMimir(),
    });

    this.addCommand({
      id: "mimir-export-vault",
      name: "Export vault to Mimir sync folder",
      callback: () => this.exportVault(),
    });

    this.addCommand({
      id: "mimir-push-note",
      name: "Push current note to Mimir",
      editorCheckCallback: (checking, editor, view) => {
        if (!checking && view?.file) {
          this.pushNoteToMimir(view.file);
        }
        return true;
      },
    });

    // Settings tab
    this.addSettingTab(new MimirSettingTab(this.app, this));

    // Status bar
    if (this.settings.showStatusBar) {
      this.statusBar = this.addStatusBarItem();
      this.statusBar.setText("Mimir: ready");
    }

    // File watcher: auto-push on save
    this.registerEvent(
      this.app.vault.on("modify", (file) => {
        if (this.isInSyncFolder(file)) {
          this.pushNoteToMimir(file);
        }
      })
    );

    this.registerEvent(
      this.app.vault.on("create", (file) => {
        if (this.isInSyncFolder(file)) {
          this.pushNoteToMimir(file);
        }
      })
    );

    // Auto-sync timer
    if (this.settings.autoSyncIntervalMinutes > 0) {
      this.startAutoSync();
    }
  }

  onunload() {
    if (this.syncInterval) {
      window.clearInterval(this.syncInterval);
    }
  }

  // ─── Sync Operations ───────────────────────────────────────────────

  /** Pull entities from Mimir database into the sync folder. */
  async pullFromMimir() {
    this.setStatus("Syncing...");
    try {
      const binary = this.settings.mimirBinaryPath;
      const dbPath = this.getDbPathArg();
      const vaultDir = this.getSyncFolderPath();

      // Run mimir_vault_export via MCP stdio
      const request = JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method: "initialize",
        params: {
          protocolVersion: "2024-11-05",
          capabilities: {},
          clientInfo: { name: "obsidian-mimir", version: "0.1.0" },
        },
      });

      const exportCall = JSON.stringify({
        jsonrpc: "2.0",
        id: 2,
        method: "tools/call",
        params: {
          name: "mimir_vault_export",
          arguments: { vault_dir: vaultDir },
        },
      });

      const input = request + "\n" + exportCall + "\n";
      const cmd = `${binary} serve ${dbPath}`;

      const result = execSync(cmd, {
        input,
        timeout: 30000,
        encoding: "utf-8",
        env: { ...process.env },
      });

      // Parse second line (the export response)
      const lines = result.trim().split("\n");
      if (lines.length >= 2) {
        const response = JSON.parse(lines[1]);
        if (response.result) {
          const content = JSON.parse(response.result.content[0].text);
          new Notice(
            `Mimir: pulled ${content.exported || "?"} entities to ${this.settings.syncFolder}`
          );
          this.setStatus(`Synced (${content.exported || "?"} entities)`);
        } else if (response.error) {
          new Notice(`Mimir sync error: ${response.error.message}`);
          this.setStatus("Error");
        }
      }
    } catch (e: any) {
      new Notice(`Mimir sync failed: ${e.message}`);
      this.setStatus("Error");
    }
  }

  /** Run mimir_vault_export to export all entities to the vault sync folder. */
  async exportVault() {
    this.setStatus("Exporting...");
    try {
      const binary = this.settings.mimirBinaryPath;
      const dbPath = this.getDbPathArg();
      const vaultDir = this.getSyncFolderPath();

      const cmd = `${binary} serve ${dbPath}`;
      const request =
        JSON.stringify({
          jsonrpc: "2.0",
          id: 1,
          method: "initialize",
          params: {
            protocolVersion: "2024-11-05",
            capabilities: {},
            clientInfo: { name: "obsidian-mimir", version: "0.1.0" },
          },
        }) +
        "\n" +
        JSON.stringify({
          jsonrpc: "2.0",
          id: 2,
          method: "tools/call",
          params: {
            name: "mimir_vault_export",
            arguments: { vault_dir: vaultDir },
          },
        }) +
        "\n";

      const result = execSync(cmd, {
        input: request,
        timeout: 30000,
        encoding: "utf-8",
      });

      const lines = result.trim().split("\n");
      if (lines.length >= 2) {
        const response = JSON.parse(lines[1]);
        if (response.result) {
          const content = JSON.parse(response.result.content[0].text);
          new Notice(`Mimir: exported ${content.exported || "?"} entities`);
          this.setStatus(`Exported ${content.exported || "?"}`);
        } else if (response.error) {
          new Notice(`Mimir export error: ${response.error.message}`);
        }
      }
    } catch (e: any) {
      new Notice(`Mimir export failed: ${e.message}`);
    }
  }

  /** Push a single Obsidian note to Mimir via mimir_remember. */
  async pushNoteToMimir(file: TFile) {
    try {
      const binary = this.settings.mimirBinaryPath;
      const dbPath = this.getDbPathArg();
      const content = await this.app.vault.read(file);

      // Extract YAML frontmatter for metadata
      const { category, key, tags } = this.parseFrontmatter(content);

      const request =
        JSON.stringify({
          jsonrpc: "2.0",
          id: 1,
          method: "initialize",
          params: {
            protocolVersion: "2024-11-05",
            capabilities: {},
            clientInfo: { name: "obsidian-mimir", version: "0.1.0" },
          },
        }) +
        "\n" +
        JSON.stringify({
          jsonrpc: "2.0",
          id: 2,
          method: "tools/call",
          params: {
            name: "mimir_remember",
            arguments: {
              id: "", // let Mimir generate
              category: category || "obsidian",
              key: key || file.basename,
              body_json: JSON.stringify({ content, source: "obsidian" }),
              type: "insight",
              status: "active",
              tags: tags || [],
              topic_path: file.parent?.path?.replace(/^\//, "") || "",
            },
          },
        }) +
        "\n";

      const result = execSync(`${binary} serve ${dbPath}`, {
        input: request,
        timeout: 10000,
        encoding: "utf-8",
      });

      this.setStatus(`Pushed: ${file.basename}`);
    } catch (e: any) {
      // Silently skip push errors to avoid noise on every save
      console.debug("Mimir push error:", e.message);
    }
  }

  // ─── Helpers ────────────────────────────────────────────────────────

  private getSyncFolderPath(): string {
    const vaultRoot = (this.app.vault.adapter as any).getBasePath?.() || "";
    return normalizePath(path.join(vaultRoot, this.settings.syncFolder));
  }

  private getDbPathArg(): string {
    return this.settings.mimirDbPath
      ? `--db "${this.settings.mimirDbPath}"`
      : "";
  }

  private isInSyncFolder(file: TFile): boolean {
    return file.path.startsWith(this.settings.syncFolder + "/");
  }

  private setStatus(text: string) {
    if (this.statusBar) {
      this.statusBar.setText(`Mimir: ${text}`);
    }
  }

  private parseFrontmatter(content: string): {
    category?: string;
    key?: string;
    tags?: string[];
  } {
    const match = content.match(/^---\n([\s\S]*?)\n---/);
    if (!match) return {};
    const fm: Record<string, any> = {};
    for (const line of match[1].split("\n")) {
      const colon = line.indexOf(":");
      if (colon > 0) {
        const key = line.slice(0, colon).trim();
        let value: any = line.slice(colon + 1).trim();
        if (value.startsWith("[") && value.endsWith("]")) {
          value = value
            .slice(1, -1)
            .split(",")
            .map((s: string) => s.trim().replace(/^"|"$/g, ""));
        }
        fm[key] = value;
      }
    }
    return {
      category: fm.category,
      key: fm.key || fm.id,
      tags: Array.isArray(fm.tags) ? fm.tags : fm.tags ? [fm.tags] : undefined,
    };
  }

  private startAutoSync() {
    this.syncInterval = window.setInterval(() => {
      this.pullFromMimir();
    }, this.settings.autoSyncIntervalMinutes * 60 * 1000);
  }

  async loadSettings() {
    this.settings = Object.assign({}, DEFAULT_SETTINGS, await this.loadData());
  }

  async saveSettings() {
    await this.saveData(this.settings);
  }
}

// ─── Settings Tab ──────────────────────────────────────────────────────

class MimirSettingTab extends PluginSettingTab {
  plugin: MimirVaultSyncPlugin;

  constructor(app: App, plugin: MimirVaultSyncPlugin) {
    super(app, plugin);
    this.plugin = plugin;
  }

  display(): void {
    const { containerEl } = this;
    containerEl.empty();

    containerEl.createEl("h2", { text: "Mimir Vault Sync Settings" });

    new Setting(containerEl)
      .setName("Mimir binary path")
      .setDesc("Path to the mimir executable")
      .addText((text) =>
        text
          .setPlaceholder("mimir")
          .setValue(this.plugin.settings.mimirBinaryPath)
          .onChange(async (value) => {
            this.plugin.settings.mimirBinaryPath = value;
            await this.plugin.saveSettings();
          })
      );

    new Setting(containerEl)
      .setName("Mimir database path")
      .setDesc("Path to the SQLite database (leave empty for default ~/.mimir/data/mimir.db)")
      .addText((text) =>
        text
          .setPlaceholder("")
          .setValue(this.plugin.settings.mimirDbPath)
          .onChange(async (value) => {
            this.plugin.settings.mimirDbPath = value;
            await this.plugin.saveSettings();
          })
      );

    new Setting(containerEl)
      .setName("Sync folder")
      .setDesc("Vault folder to sync with Mimir (created if missing)")
      .addText((text) =>
        text
          .setPlaceholder("mimir")
          .setValue(this.plugin.settings.syncFolder)
          .onChange(async (value) => {
            this.plugin.settings.syncFolder = value;
            await this.plugin.saveSettings();
          })
      );

    new Setting(containerEl)
      .setName("Auto-sync interval (minutes)")
      .setDesc("0 = disabled. Automatically pulls from Mimir on this interval.")
      .addText((text) =>
        text
          .setPlaceholder("0")
          .setValue(String(this.plugin.settings.autoSyncIntervalMinutes))
          .onChange(async (value) => {
            const v = parseInt(value) || 0;
            this.plugin.settings.autoSyncIntervalMinutes = v;
            await this.plugin.saveSettings();
            if (v > 0) {
              this.plugin.startAutoSync();
            } else if (this.plugin.syncInterval) {
              window.clearInterval(this.plugin.syncInterval);
              this.plugin.syncInterval = null;
            }
          })
      );

    new Setting(containerEl)
      .setName("Show status bar")
      .setDesc("Display sync status in the Obsidian status bar")
      .addToggle((toggle) =>
        toggle
          .setValue(this.plugin.settings.showStatusBar)
          .onChange(async (value) => {
            this.plugin.settings.showStatusBar = value;
            await this.plugin.saveSettings();
          })
      );
  }
}
