// Typed wrappers around the Rust commands exposed by the Tauri backend.
import { invoke } from "@tauri-apps/api/core";

export interface AudioDevice {
  id: string;
  name: string;
  state: "Active" | "Disabled" | "NotPresent" | "Unplugged" | "Unknown";
  volume: number; // 0.0 - 1.0
  muted: boolean;
  is_default_audio: boolean;
  is_default_comm: boolean;
}

export interface ProfileItem {
  app: string;
  device_id: string;
  device_name: string;
}

export interface RoutingRule {
  match_exe: string;
  target_device_id: string | null; // null -> system default
  target_device_name: string;
  is_comms: boolean;
  enabled: boolean;
}

export interface AppSession {
  exe: string;
  display_name: string;
  volume: number; // 0.0 - 1.0
  muted: boolean;
  session_count: number;
}

export interface VolumeRule {
  match_exe: string;
  volume: number; // 0.0 - 1.0
  muted: boolean;
  enabled: boolean;
}

export const api = {
  listDevices: () => invoke<AudioDevice[]>("list_devices"),
  setDefault: (deviceId: string) => invoke<void>("set_default", { deviceId }),
  setVolume: (deviceId: string, volume: number) =>
    invoke<void>("set_volume", { deviceId, volume }),
  setMute: (deviceId: string, mute: boolean) =>
    invoke<void>("set_mute", { deviceId, mute }),
  peaks: (deviceIds: string[]) => invoke<number[]>("peaks", { deviceIds }),

  getProfiles: () => invoke<ProfileItem[]>("get_profiles"),
  addProfile: (appName: string, deviceId: string, deviceName: string) =>
    invoke<ProfileItem[]>("add_profile", { appName, deviceId, deviceName }),
  removeProfile: (appName: string) =>
    invoke<ProfileItem[]>("remove_profile", { appName }),

  getHotkey: () => invoke<string>("get_hotkey"),
  setHotkey: (hotkey: string) => invoke<void>("set_hotkey", { hotkey }),

  hideHud: () => invoke<void>("hide_hud"),

  // v2 per-session routing
  routingAvailable: () => invoke<boolean>("routing_available"),
  getRoutingRules: () => invoke<RoutingRule[]>("get_routing_rules"),
  setRoutingRule: (rule: RoutingRule) =>
    invoke<RoutingRule[]>("set_routing_rule", { rule }),
  removeRoutingRule: (matchExe: string) =>
    invoke<RoutingRule[]>("remove_routing_rule", { matchExe }),
  clearRouting: () => invoke<void>("clear_routing"),

  // per-app volume
  listAppSessions: () => invoke<AppSession[]>("list_app_sessions"),
  setAppVolume: (exe: string, volume: number) =>
    invoke<void>("set_app_volume", { exe, volume }),
  setAppMute: (exe: string, mute: boolean) =>
    invoke<void>("set_app_mute", { exe, mute }),
  getVolumeRules: () => invoke<VolumeRule[]>("get_volume_rules"),
  setVolumeRule: (rule: VolumeRule) =>
    invoke<VolumeRule[]>("set_volume_rule", { rule }),
  removeVolumeRule: (matchExe: string) =>
    invoke<VolumeRule[]>("remove_volume_rule", { matchExe }),
};
