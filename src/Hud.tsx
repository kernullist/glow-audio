import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "./api";

interface HudPayload {
  title: string;
  subtitle: string;
  volume: number;
}

// Floating, borderless, translucent overlay that fades in on a device switch,
// holds briefly, then fades out and asks the backend to hide its window.
export default function Hud() {
  const [data, setData] = useState<HudPayload>({
    title: "GlowAudio Active",
    subtitle: "",
    volume: 0,
  });
  const [visible, setVisible] = useState(false);
  const hideTimer = useRef<number | null>(null);
  const closeTimer = useRef<number | null>(null);

  useEffect(() => {
    const unlisten = listen<HudPayload>("hud-update", (event) => {
      setData(event.payload);
      setVisible(true);

      if (hideTimer.current) window.clearTimeout(hideTimer.current);
      if (closeTimer.current) window.clearTimeout(closeTimer.current);

      // Hold visible for 2.2s, then trigger the fade-out.
      hideTimer.current = window.setTimeout(() => setVisible(false), 2200);
      // After the fade transition completes, hide the OS window entirely.
      closeTimer.current = window.setTimeout(() => {
        void api.hideHud();
      }, 2700);
    });

    return () => {
      void unlisten.then((f) => f());
      if (hideTimer.current) window.clearTimeout(hideTimer.current);
      if (closeTimer.current) window.clearTimeout(closeTimer.current);
    };
  }, []);

  const isHeadset = /head|ear/i.test(data.subtitle);

  return (
    <div className={`hud-root ${visible ? "hud-show" : "hud-hide"}`}>
      <div className="hud-card">
        <div className="hud-icon">{isHeadset ? "🎧" : "🔊"}</div>
        <div className="hud-text">
          <div className="hud-title">{data.title}</div>
          <div className="hud-device">{data.subtitle}</div>
          <div className="hud-vol">Volume: {data.volume}%</div>
        </div>
      </div>
    </div>
  );
}
