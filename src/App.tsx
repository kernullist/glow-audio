import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, AudioDevice, ProfileItem } from "./api";

type Tab = "devices" | "profiles" | "settings";

export default function App() {
  const [tab, setTab] = useState<Tab>("devices");
  const [hotkey, setHotkey] = useState<string>("");

  // Load the current global hotkey once so the sidebar can display the binding.
  useEffect(() => {
    void api.getHotkey().then(setHotkey);
  }, []);

  return (
    <div className="app">
      <Sidebar tab={tab} onTab={setTab} hotkey={hotkey} />
      <main className="workspace">
        {tab === "devices" && <DevicesView />}
        {tab === "profiles" && <ProfilesView />}
        {tab === "settings" && (
          <SettingsView hotkey={hotkey} onHotkeyChange={setHotkey} />
        )}
      </main>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Sidebar
// ---------------------------------------------------------------------------

function Sidebar({
  tab,
  onTab,
  hotkey,
}: {
  tab: Tab;
  onTab: (t: Tab) => void;
  hotkey: string;
}) {
  const items: { key: Tab; label: string; icon: string }[] = [
    { key: "devices", label: "Playback Devices", icon: "🔊" },
    { key: "profiles", label: "Game Profiles", icon: "🎮" },
    { key: "settings", label: "Global Settings", icon: "⚙️" },
  ];

  return (
    <aside className="sidebar">
      <div className="brand">✨ GlowAudio</div>
      <div className="brand-sub">Smart Audio Router</div>

      <nav className="nav">
        {items.map((it) => (
          <button
            key={it.key}
            className={`nav-btn ${tab === it.key ? "nav-active" : ""}`}
            onClick={() => onTab(it.key)}
          >
            <span className="nav-icon">{it.icon}</span>
            {it.label}
          </button>
        ))}
      </nav>

      <div className="engine-panel">
        <div className="engine-title">STATUS</div>
        <div className="engine-row">
          <span className="dot dot-on" /> Game Auto-Switch
        </div>
        <div className="engine-row engine-row-split">
          <span className="engine-label">
            <span className="dot dot-on" /> Hotkey
          </span>
          <span className="hotkey-chip">{formatHotkey(hotkey)}</span>
        </div>
      </div>
    </aside>
  );
}

// Render a hotkey accelerator string as compact key tokens, e.g.
// "Ctrl+Shift+A" -> "Ctrl + Shift + A". Falls back to a dash when unset.
function formatHotkey(hotkey: string): string {
  if (!hotkey) {
    return "—";
  }
  return hotkey
    .split("+")
    .map((k) => k.trim())
    .filter(Boolean)
    .join("+");
}

// ---------------------------------------------------------------------------
// Devices tab
// ---------------------------------------------------------------------------

function DevicesView() {
  const [devices, setDevices] = useState<AudioDevice[]>([]);
  const [peakMap, setPeakMap] = useState<Record<string, number>>({});
  const activeIdsRef = useRef<string[]>([]);

  const refresh = useCallback(async () => {
    try {
      const list = await api.listDevices();
      const shown = list.filter(
        (d) => d.state === "Active" || d.state === "Unplugged"
      );
      setDevices(shown);
      activeIdsRef.current = shown
        .filter((d) => d.state === "Active")
        .map((d) => d.id);
    } catch (e) {
      console.error(e);
    }
  }, []);

  // Initial load + auto-refresh + react to backend switch events.
  useEffect(() => {
    void refresh();
    const auto = window.setInterval(refresh, 6000);
    const unlisten = listen("devices-changed", () => void refresh());
    return () => {
      window.clearInterval(auto);
      void unlisten.then((f) => f());
    };
  }, [refresh]);

  // Peak meter polling. Guarded so we never overlap a slow COM query with the
  // next tick, and paused while the window is hidden (e.g. minimized to tray)
  // to avoid needless enumeration.
  useEffect(() => {
    let inFlight = false;
    const timer = window.setInterval(async () => {
      if (inFlight || document.hidden) return;
      const ids = activeIdsRef.current;
      if (ids.length === 0) return;
      inFlight = true;
      try {
        const values = await api.peaks(ids);
        const next: Record<string, number> = {};
        ids.forEach((id, i) => (next[id] = values[i] ?? 0));
        setPeakMap(next);
      } catch {
        /* ignore transient COM errors */
      } finally {
        inFlight = false;
      }
    }, 100);
    return () => window.clearInterval(timer);
  }, []);

  return (
    <div className="view">
      <header className="view-head">
        <h1>Audio Output Devices</h1>
        <button className="btn-ghost" onClick={() => void refresh()}>
          🔄 Refresh
        </button>
      </header>

      <div className="scroll">
        {devices.length === 0 && (
          <div className="empty">No connected playback devices found.</div>
        )}
        {devices.map((d) => (
          <DeviceCard
            key={d.id}
            device={d}
            peak={peakMap[d.id] ?? 0}
            onChanged={refresh}
          />
        ))}
      </div>
    </div>
  );
}

function DeviceCard({
  device,
  peak,
  onChanged,
}: {
  device: AudioDevice;
  peak: number;
  onChanged: () => void;
}) {
  const [vol, setVol] = useState(Math.round(device.volume * 100));
  const isHeadset = /head|ear/i.test(device.name);
  const active = device.is_default_audio;

  useEffect(() => {
    setVol(Math.round(device.volume * 100));
  }, [device.volume]);

  const onVolume = (value: number) => {
    setVol(value);
    void api.setVolume(device.id, value / 100);
  };

  const makeDefault = async () => {
    await api.setDefault(device.id);
    onChanged();
  };

  const toggleMute = async () => {
    await api.setMute(device.id, !device.muted);
    onChanged();
  };

  return (
    <div className={`card ${active ? "card-active" : ""}`}>
      <div className="card-icon">{isHeadset ? "🎧" : "🔊"}</div>

      <div className="card-main">
        <div className="card-name">{device.name}</div>
        <div className="badges">
          <span className={device.state === "Active" ? "tag-on" : "tag-off"}>
            {device.state}
          </span>
          {active && <span className="tag-default">● Default Sound</span>}
          {device.is_default_comm && (
            <span className="tag-comm">● Communications</span>
          )}
        </div>
        <div className="meter">
          <div
            className="meter-fill"
            style={{ width: `${Math.min(100, Math.round(peak * 100))}%` }}
          />
        </div>
      </div>

      <div className="card-controls">
        <button
          className="btn-icon"
          title={device.muted ? "Unmute" : "Mute"}
          onClick={() => void toggleMute()}
        >
          {device.muted ? "🔇" : "🔊"}
        </button>
        <input
          type="range"
          min={0}
          max={100}
          value={vol}
          onChange={(e) => onVolume(Number(e.target.value))}
          className="slider"
        />
        <span className="vol-label">{vol}%</span>
        <button
          className={`btn-default ${active ? "btn-default-on" : ""}`}
          disabled={active}
          onClick={() => void makeDefault()}
        >
          {active ? "Default" : "Set Default"}
        </button>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Profiles tab
// ---------------------------------------------------------------------------

function ProfilesView() {
  const [profiles, setProfiles] = useState<ProfileItem[]>([]);
  const [devices, setDevices] = useState<AudioDevice[]>([]);
  const [appName, setAppName] = useState("");
  const [deviceId, setDeviceId] = useState("");

  const refresh = useCallback(async () => {
    const [p, list] = await Promise.all([api.getProfiles(), api.listDevices()]);
    setProfiles(p);
    const active = list.filter((d) => d.state === "Active");
    setDevices(active);
    setDeviceId((prev) =>
      active.some((d) => d.id === prev) ? prev : active[0]?.id ?? ""
    );
  }, []);

  useEffect(() => {
    void refresh();
    const unlisten = listen("devices-changed", () => void refresh());
    return () => void unlisten.then((f) => f());
  }, [refresh]);

  const add = async () => {
    const name = appName.trim();
    const dev = devices.find((d) => d.id === deviceId);
    if (!name || !dev) return;
    const updated = await api.addProfile(name, dev.id, dev.name);
    setProfiles(updated);
    setAppName("");
  };

  const remove = async (app: string) => {
    setProfiles(await api.removeProfile(app));
  };

  return (
    <div className="view">
      <header className="view-head">
        <h1>Game Output Profiles</h1>
      </header>

      <div className="add-card">
        <div className="add-title">Add Automatic Application Routing Profile</div>
        <div className="add-row">
          <input
            className="text-input"
            placeholder="e.g. valorant.exe or notepad.exe"
            value={appName}
            onChange={(e) => setAppName(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && void add()}
          />
          <select
            className="select"
            value={deviceId}
            onChange={(e) => setDeviceId(e.target.value)}
          >
            {devices.length === 0 && <option value="">No Active Devices</option>}
            {devices.map((d) => (
              <option key={d.id} value={d.id}>
                {d.name}
              </option>
            ))}
          </select>
          <button className="btn-primary" onClick={() => void add()}>
            ➕ Map Game
          </button>
        </div>
      </div>

      <div className="list-title">Active Application Routing Profiles</div>
      <div className="scroll">
        {profiles.length === 0 && (
          <div className="empty">No application routing profiles registered yet.</div>
        )}
        {profiles.map((p) => (
          <div className="profile-row" key={p.app}>
            <span className="profile-icon">🎮</span>
            <span className="profile-app">{p.app}</span>
            <span className="profile-arrow">➡️</span>
            <span className="profile-dev">{p.device_name}</span>
            <button className="btn-del" onClick={() => void remove(p.app)}>
              🗑️ Delete
            </button>
          </div>
        ))}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Settings tab
// ---------------------------------------------------------------------------

function SettingsView({
  hotkey,
  onHotkeyChange,
}: {
  hotkey: string;
  onHotkeyChange: (h: string) => void;
}) {
  const [draft, setDraft] = useState(hotkey);
  const [status, setStatus] = useState<string | null>(null);

  // Keep the editable draft in sync if the shared hotkey changes elsewhere.
  useEffect(() => {
    setDraft(hotkey);
  }, [hotkey]);

  const save = async () => {
    const value = draft.trim();
    try {
      await api.setHotkey(value);
      onHotkeyChange(value);
      setStatus("Saved and re-registered.");
    } catch (e) {
      setStatus(String(e));
    }
    window.setTimeout(() => setStatus(null), 3000);
  };

  return (
    <div className="view">
      <header className="view-head">
        <h1>Global Configuration & Shortcuts</h1>
      </header>

      <div className="settings-card">
        <div className="settings-label">Global Cycle Hotkey</div>
        <p className="settings-desc">
          Pressing this combination in the background cycles between your active
          audio devices and shows the floating HUD overlay.
        </p>
        <div className="add-row">
          <input
            className="text-input"
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
          />
          <button className="btn-primary" onClick={() => void save()}>
            Save Hotkey
          </button>
        </div>
        {status && <div className="status-line">{status}</div>}

        <div className="info-panel">
          <div className="info-title">Supported Hotkey Syntax</div>
          <div className="info-body">
            • Modifiers + a key, joined by "+": <code>Ctrl+Shift+A</code>
            <br />• Modifiers: <code>Ctrl</code>, <code>Shift</code>,{" "}
            <code>Alt</code>, <code>Super</code>
            <br />• Example: <code>Ctrl+Alt+S</code>
          </div>
        </div>
      </div>
    </div>
  );
}
