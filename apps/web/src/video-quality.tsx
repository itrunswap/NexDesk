import { useEffect, useRef, useState } from "react";
import type { RemoteSession } from "./protocol";

const profiles = { auto: "自动", clear: "文字清晰", smooth: "操作流畅" };
const resolutions = {
  hd: "720p",
  full_hd: "1080p",
  qhd: "1440p · 2K",
  uhd: "2160p · 4K",
};
type Quality = {
  preset: keyof typeof profiles;
  fps: number;
  resolution?: keyof typeof resolutions;
  max_mbps?: number;
};
const initial: Quality = { preset: "auto", fps: 30 };

function quality(value: unknown): Quality | undefined {
  if (!value || typeof value !== "object") return;
  const input = value as Partial<Quality>;
  if (
    !input.preset ||
    !Object.hasOwn(profiles, input.preset) ||
    typeof input.fps !== "number" ||
    !Number.isInteger(input.fps) ||
    input.fps < 10 ||
    input.fps > 60 ||
    (input.resolution != null &&
      !Object.hasOwn(resolutions, input.resolution)) ||
    (input.max_mbps != null &&
      (!Number.isInteger(input.max_mbps) ||
        input.max_mbps < 1 ||
        input.max_mbps > 80))
  )
    return;
  return {
    preset: input.preset,
    fps: input.fps,
    resolution: input.resolution ?? undefined,
    max_mbps: input.max_mbps ?? undefined,
  };
}

async function send(session: RemoteSession, next: Quality) {
  const { value } = await session.collaboration.request<{
    kind: string;
    quality?: unknown;
  }>({
    kind: "quality",
    quality: next,
  });
  const applied = value.kind === "quality" && quality(value.quality);
  if (!applied) throw new Error("被控端未确认画质设置。");
  return applied;
}

export function VideoQualityControls({ session }: { session: RemoteSession }) {
  const [current, setCurrent] = useState<Quality>(initial);
  const [busy, setBusy] = useState(true);
  const [notice, setNotice] = useState("");
  const generation = useRef(0);
  useEffect(() => {
    const currentGeneration = ++generation.current;
    const active = () => currentGeneration === generation.current;
    setCurrent(initial);
    setNotice("");
    setBusy(true);
    const detach = session.collaboration.onOpen(() => {
      void (async () => {
        let stored: Quality | undefined;
        try {
          stored = quality(
            JSON.parse(localStorage.getItem(session.qualityKey) ?? "null"),
          );
        } catch {
          /* storage is optional */
        }
        try {
          const applied = await send(session, stored ?? initial);
          if (active()) setCurrent(applied);
        } catch {
          if (active()) setNotice("画质设置暂未确认，可重新选择。");
        } finally {
          if (active()) setBusy(false);
        }
      })();
    });
    return () => {
      generation.current++;
      detach();
    };
  }, [session]);

  async function change(next: Quality) {
    const request = generation.current;
    setBusy(true);
    setNotice("");
    try {
      const applied = await send(session, next);
      if (request !== generation.current) return;
      setCurrent(applied);
      if (
        applied.resolution !== next.resolution ||
        applied.max_mbps !== next.max_mbps
      )
        setNotice("被控端仅确认了部分上限，已显示实际设置。");
      try {
        localStorage.setItem(session.qualityKey, JSON.stringify(applied));
      } catch {
        /* private browsing */
      }
    } catch (error) {
      if (request === generation.current)
        setNotice(error instanceof Error ? error.message : "画质调整失败。");
    } finally {
      if (request === generation.current) setBusy(false);
    }
  }
  const options = (values: number[], selected?: number) =>
    [...new Set(selected ? [...values, selected] : values)].sort(
      (a, b) => a - b,
    );
  return (
    <>
      <label>
        画质{" "}
        <select
          disabled={busy}
          value={current.preset}
          onChange={(e) =>
            void change({
              ...current,
              preset: e.target.value as Quality["preset"],
            })
          }
        >
          {Object.entries(profiles).map(([key, label]) => (
            <option key={key} value={key}>
              {label}
            </option>
          ))}
        </select>
      </label>
      <label>
        传输分辨率上限{" "}
        <select
          disabled={busy}
          value={current.resolution ?? ""}
          onChange={(e) =>
            void change({
              ...current,
              resolution: (e.target.value ||
                undefined) as Quality["resolution"],
            })
          }
        >
          <option value="">按远端尺寸</option>
          {Object.entries(resolutions).map(([key, label]) => (
            <option key={key} value={key}>
              {label}
            </option>
          ))}
        </select>
      </label>
      <label>
        帧率上限{" "}
        <select
          disabled={busy}
          value={current.fps}
          onChange={(e) =>
            void change({ ...current, fps: Number(e.target.value) })
          }
        >
          {options([10, 15, 24, 30, 45, 60], current.fps).map((rate) => (
            <option key={rate} value={rate}>
              {rate} FPS
            </option>
          ))}
        </select>
      </label>
      <label>
        码率上限{" "}
        <select
          disabled={busy}
          value={current.max_mbps ?? ""}
          onChange={(e) =>
            void change({
              ...current,
              max_mbps: Number(e.target.value) || undefined,
            })
          }
        >
          <option value="">自动</option>
          {options([6, 8, 12, 16, 24, 32, 48, 64, 80], current.max_mbps).map(
            (rate) => (
              <option key={rate} value={rate}>
                {rate} Mbps
              </option>
            ),
          )}
        </select>
      </label>
      {notice && <output role="status">{notice}</output>}
    </>
  );
}
