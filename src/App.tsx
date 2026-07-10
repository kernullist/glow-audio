import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  api,
  AppSession,
  AudioDevice,
  ProfileItem,
  RoutingRule,
  VolumeRule,
} from "./api";

type Tab = "devices" | "profiles" | "routing" | "volume" | "settings";

// Throttle a numeric send to at most one call per `ms`, always flushing the
// latest value at the end of the window. Range inputs fire onChange dozens of
// times per second while dragging, and every backend call enumerates COM
// devices/sessions - unthrottled, a single drag floods the audio engine.
function useThrottledSend(send: (v: number) => void, ms = 80) {
  const sendRef = useRef(send);
  sendRef.current = send;
  const timer = useRef<number | null>(null);
  const pending = useRef<number | null>(null);

  useEffect(() => {
    return () => {
      if (timer.current !== null) window.clearTimeout(timer.current);
    };
  }, []);

  return useCallback(
    (value: number) => {
      if (timer.current === null) {
        sendRef.current(value);
        timer.current = window.setTimeout(() => {
          timer.current = null;
          if (pending.current !== null) {
            sendRef.current(pending.current);
            pending.current = null;
          }
        }, ms);
      } else {
        pending.current = value;
      }
    },
    [ms]
  );
}

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
        {tab === "routing" && <RoutingView />}
        {tab === "volume" && <VolumeView />}
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
    { key: "routing", label: "App Routing", icon: "🎚️" },
    { key: "volume", label: "App Volume", icon: "🎛️" },
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

  const pushVolume = useThrottledSend((v) => void api.setVolume(device.id, v / 100));

  const onVolume = (value: number) => {
    setVol(value);
    pushVolume(value);
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
// App Routing tab (v2 per-session routing)
// ---------------------------------------------------------------------------

function RoutingView() {
  const [available, setAvailable] = useState<boolean | null>(null);
  const [rules, setRules] = useState<RoutingRule[]>([]);
  const [devices, setDevices] = useState<AudioDevice[]>([]);
  const [appName, setAppName] = useState("");
  const [deviceId, setDeviceId] = useState(""); // "" -> system default
  const [isComms, setIsComms] = useState(false);

  const refresh = useCallback(async () => {
    const [avail, ruleList, list] = await Promise.all([
      api.routingAvailable(),
      api.getRoutingRules(),
      api.listDevices(),
    ]);
    setAvailable(avail);
    setRules(ruleList);
    setDevices(list.filter((d) => d.state === "Active"));
  }, []);

  useEffect(() => {
    void refresh();
    const un1 = listen("routing-available", () => setAvailable(true));
    const un2 = listen("routing-unavailable", () => setAvailable(false));
    // Keep the device dropdown current when endpoints are hot-plugged.
    const un3 = listen("devices-changed", () => void refresh());
    return () => {
      void un1.then((f) => f());
      void un2.then((f) => f());
      void un3.then((f) => f());
    };
  }, [refresh]);

  const add = async () => {
    const name = appName.trim();
    if (!name) return;
    const dev = devices.find((d) => d.id === deviceId);
    const rule: RoutingRule = {
      match_exe: name,
      target_device_id: deviceId === "" ? null : deviceId,
      target_device_name: deviceId === "" ? "System default" : dev?.name ?? "",
      is_comms: isComms,
      enabled: true,
    };
    setRules(await api.setRoutingRule(rule));
    setAppName("");
    setIsComms(false);
  };

  const remove = async (exe: string) => {
    setRules(await api.removeRoutingRule(exe));
  };

  const reset = async () => {
    await api.clearRouting();
    setRules([]);
  };

  return (
    <div className="view">
      <header className="view-head">
        <h1>Per-App Audio Routing</h1>
        {rules.length > 0 && (
          <button className="btn-del" onClick={() => void reset()}>
            ♻️ Reset routing
          </button>
        )}
      </header>

      {available === false && (
        <div className="notice">
          Per-app routing isn't available on this Windows build. The app falls
          back to default-device switching (Playback Devices / Game Profiles).
        </div>
      )}

      <div className="add-card">
        <div className="add-title">Route an application to a device</div>
        <div className="add-row">
          <input
            className="text-input"
            placeholder="e.g. chrome.exe or discord.exe"
            value={appName}
            onChange={(e) => setAppName(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && void add()}
            disabled={available === false}
          />
          <select
            className="select"
            value={deviceId}
            onChange={(e) => setDeviceId(e.target.value)}
            disabled={available === false}
          >
            <option value="">System default</option>
            {devices.map((d) => (
              <option key={d.id} value={d.id}>
                {d.name}
              </option>
            ))}
          </select>
          <label className="comms-toggle">
            <input
              type="checkbox"
              checked={isComms}
              onChange={(e) => setIsComms(e.target.checked)}
              disabled={available === false}
            />
            Comms
          </label>
          <button
            className="btn-primary"
            onClick={() => void add()}
            disabled={available === false}
          >
            ➕ Add Rule
          </button>
        </div>
      </div>

      <div className="list-title">Active Routing Rules</div>
      <div className="scroll">
        {rules.length === 0 && (
          <div className="empty">
            No per-app routing rules yet. Add one above to send an app's audio to
            a specific device while others stay on the system default.
          </div>
        )}
        {rules.map((r) => (
          <div className="profile-row" key={r.match_exe}>
            <span className="profile-icon">🎚️</span>
            <span className="profile-app">{r.match_exe}</span>
            <span className="profile-arrow">➡️</span>
            <span className="profile-dev">
              {r.target_device_id ? r.target_device_name : "System default"}
            </span>
            {r.is_comms && <span className="tag-comm">● Comms</span>}
            <button className="btn-del" onClick={() => void remove(r.match_exe)}>
              🗑️ Delete
            </button>
          </div>
        ))}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// App Volume tab (per-app session volume + remembered levels)
// ---------------------------------------------------------------------------

function VolumeView() {
  const [sessions, setSessions] = useState<AppSession[]>([]);
  const [rules, setRules] = useState<VolumeRule[]>([]);

  const refresh = useCallback(async () => {
    const [s, r] = await Promise.all([
      api.listAppSessions(),
      api.getVolumeRules(),
    ]);
    setSessions(s);
    setRules(r);
  }, []);

  useEffect(() => {
    void refresh();
    const t = window.setInterval(() => {
      if (!document.hidden) void refresh();
    }, 1500);
    return () => window.clearInterval(t);
  }, [refresh]);

  const remembered = (exe: string) => rules.some((r) => r.match_exe === exe);

  // volume/muted come from the row's live controls, not the last poll, so the
  // value saved is exactly what the user just set.
  const toggleRemember = async (exe: string, volume: number, muted: boolean) => {
    if (remembered(exe)) {
      setRules(await api.removeVolumeRule(exe));
    } else {
      setRules(
        await api.setVolumeRule({ match_exe: exe, volume, muted, enabled: true })
      );
    }
  };

  // Keep the remembered value in sync while the app stays checked.
  const syncRule = async (exe: string, volume: number, muted: boolean) => {
    if (remembered(exe)) {
      await api.setVolumeRule({ match_exe: exe, volume, muted, enabled: true });
    }
  };

  return (
    <div className="view">
      <header className="view-head">
        <h1>Per-App Volume</h1>
        <button className="btn-ghost" onClick={() => void refresh()}>
          🔄 Refresh
        </button>
      </header>

      <p className="settings-desc">
        Adjust the volume of each app that's currently playing audio. Tick{" "}
        <b>Remember</b> and GlowAudio re-applies that level automatically the next
        time the app starts.
      </p>

      <div className="scroll">
        {sessions.length === 0 && (
          <div className="empty">No apps are playing audio right now.</div>
        )}
        {sessions.map((s) => (
          <SessionRow
            key={s.exe}
            session={s}
            remembered={remembered(s.exe)}
            onRemember={(volume, muted) =>
              void toggleRemember(s.exe, volume, muted)
            }
            onAfterChange={syncRule}
          />
        ))}

        {rules.length > 0 && (
          <>
            <div className="list-title" style={{ marginTop: 18 }}>
              Remembered Apps
            </div>
            <div className="remembered-list">
              {rules.map((r) => (
                <span className="remembered-chip" key={r.match_exe}>
                  {r.match_exe} · {Math.round(r.volume * 100)}%
                  {r.muted ? " · muted" : ""}
                  <button
                    className="chip-x"
                    title="Forget"
                    onClick={() =>
                      void api.removeVolumeRule(r.match_exe).then(setRules)
                    }
                  >
                    ✕
                  </button>
                </span>
              ))}
            </div>
          </>
        )}
      </div>
    </div>
  );
}

function SessionRow({
  session,
  remembered,
  onRemember,
  onAfterChange,
}: {
  session: AppSession;
  remembered: boolean;
  onRemember: (volume: number, muted: boolean) => void;
  onAfterChange: (exe: string, volume: number, muted: boolean) => void;
}) {
  const [vol, setVol] = useState(Math.round(session.volume * 100));
  const volRef = useRef(vol);

  useEffect(() => {
    setVol(Math.round(session.volume * 100));
    volRef.current = Math.round(session.volume * 100);
  }, [session.volume]);

  const pushVolume = useThrottledSend(
    (v) => void api.setAppVolume(session.exe, v / 100)
  );

  const onVolume = (value: number) => {
    setVol(value);
    volRef.current = value;
    pushVolume(value);
  };

  // Persist the remembered rule once per adjustment (drag release / key up),
  // not on every slider tick - each save writes to disk and pings the worker.
  const commitRule = () => {
    void onAfterChange(session.exe, volRef.current / 100, session.muted);
  };

  const toggleMute = async () => {
    await api.setAppMute(session.exe, !session.muted);
    // Sync with the live slider value, not the last-polled session volume.
    void onAfterChange(session.exe, volRef.current / 100, !session.muted);
  };

  return (
    <div className={`card ${remembered ? "card-active" : ""}`}>
      <div className="card-icon">{session.muted ? "🔇" : "🔊"}</div>

      <div className="card-main">
        <div className="card-name">
          {session.display_name}
          {session.session_count > 1 ? ` (${session.session_count})` : ""}
        </div>
        <div className="badges">
          <span className="session-exe">{session.exe}</span>
        </div>
      </div>

      <div className="card-controls">
        <button
          className="btn-icon"
          title={session.muted ? "Unmute" : "Mute"}
          onClick={() => void toggleMute()}
        >
          {session.muted ? "🔇" : "🔊"}
        </button>
        <input
          type="range"
          min={0}
          max={100}
          value={vol}
          onChange={(e) => onVolume(Number(e.target.value))}
          onPointerUp={commitRule}
          onKeyUp={commitRule}
          className="slider"
        />
        <span className="vol-label">{vol}%</span>
        <label className="comms-toggle">
          <input
            type="checkbox"
            checked={remembered}
            onChange={() => onRemember(vol / 100, session.muted)}
          />
          Remember
        </label>
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
  const [autostart, setAutostart] = useState(false);

  // Keep the editable draft in sync if the shared hotkey changes elsewhere.
  useEffect(() => {
    setDraft(hotkey);
  }, [hotkey]);

  useEffect(() => {
    void api.getAutostart().then(setAutostart);
  }, []);

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

  const toggleAutostart = async () => {
    const next = !autostart;
    try {
      await api.setAutostart(next);
      setAutostart(next);
    } catch (e) {
      setStatus(String(e));
      window.setTimeout(() => setStatus(null), 3000);
    }
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

        <div className="settings-label" style={{ marginTop: 24 }}>
          Startup
        </div>
        <p className="settings-desc">
          GlowAudio only auto-switches devices and restores app volumes while it
          is running. Enable this to start it (in the tray) when you log in.
        </p>
        <label className="comms-toggle">
          <input
            type="checkbox"
            checked={autostart}
            onChange={() => void toggleAutostart()}
          />
          Start GlowAudio when Windows starts
        </label>

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
