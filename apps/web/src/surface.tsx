import { useEffect, useMemo, useRef, useState } from "react";
import type { KeyboardEvent, PointerEvent } from "react";
import { createPortal } from "react-dom";
import {
  ActionKind,
  Capability,
  type DesktopState,
  type RemoteSession,
} from "./protocol";
import {
  TouchInput,
  videoBounds,
  type Point,
  type TouchMode,
  type Viewport,
} from "./touch";

const usages: Record<string, number> = {
  Enter: 0x28,
  Escape: 0x29,
  Backspace: 0x2a,
  Tab: 0x2b,
  Space: 0x2c,
  Minus: 0x2d,
  Equal: 0x2e,
  BracketLeft: 0x2f,
  BracketRight: 0x30,
  Backslash: 0x31,
  Semicolon: 0x33,
  Quote: 0x34,
  Backquote: 0x35,
  Comma: 0x36,
  Period: 0x37,
  Slash: 0x38,
  CapsLock: 0x39,
  PrintScreen: 0x46,
  ScrollLock: 0x47,
  Pause: 0x48,
  Insert: 0x49,
  Home: 0x4a,
  PageUp: 0x4b,
  Delete: 0x4c,
  End: 0x4d,
  PageDown: 0x4e,
  ArrowRight: 0x4f,
  ArrowLeft: 0x50,
  ArrowDown: 0x51,
  ArrowUp: 0x52,
  NumLock: 0x53,
  NumpadDivide: 0x54,
  NumpadMultiply: 0x55,
  NumpadSubtract: 0x56,
  NumpadAdd: 0x57,
  NumpadEnter: 0x58,
  NumpadDecimal: 0x63,
  ContextMenu: 0x65,
  IntlBackslash: 0x64,
  IntlRo: 0x87,
  IntlYen: 0x89,
  ControlLeft: 0xe0,
  ShiftLeft: 0xe1,
  AltLeft: 0xe2,
  MetaLeft: 0xe3,
  ControlRight: 0xe4,
  ShiftRight: 0xe5,
  AltRight: 0xe6,
  MetaRight: 0xe7,
};

function hid(key: string) {
  let usage = usages[key];
  if (/^Key[A-Z]$/u.test(key)) usage = 4 + key.charCodeAt(3) - 65;
  if (/^Digit[0-9]$/u.test(key)) usage = 0x1e + ((Number(key[5]) + 9) % 10);
  if (/^Numpad[0-9]$/u.test(key)) usage = 0x59 + ((Number(key[6]) + 9) % 10);
  if (/^F(?:[1-9]|1[0-9]|2[0-4])$/u.test(key)) {
    const number = Number(key.slice(1));
    usage = number <= 12 ? 0x39 + number : 0x68 + number - 13;
  }
  return usage === undefined ? undefined : 0x70000 + usage;
}

const buttons = [1, 3, 2, 4, 5];
const clamp = (value: number, limit: number) =>
  Math.max(0, Math.min(limit, Math.round(value)));

export function Surface({
  session,
  state,
  scale,
  volume,
  mobile = false,
  controls,
  onToolAction,
}: {
  session: RemoteSession;
  state: DesktopState | undefined;
  scale: "fit" | "actual" | "double";
  volume: number;
  mobile?: boolean;
  controls: HTMLElement | null;
  onToolAction: () => void;
}) {
  const area = useRef<HTMLDivElement>(null);
  const viewport = useRef<HTMLDivElement>(null);
  const video = useRef<HTMLVideoElement>(null);
  const [decoded, setDecoded] = useState({ width: 0, height: 0 });
  const [ratio, setRatio] = useState(() => window.devicePixelRatio || 1);
  const [viewportSize, setViewportSize] = useState({ x: 0, y: 0 });
  const pressed = useRef(new Set<number>());
  const held = useRef(new Set<number>());
  const lastMove = useRef(0);
  const canInput = state?.can_input && session.grant === "control";
  const [soundIssue, setSoundIssue] = useState(false);
  const [mode, setMode] = useState<TouchMode>("trackpad");
  const [view, setView] = useState<Viewport>({ zoom: 1, x: 0, y: 0 });
  const [drag, setDrag] = useState(false);
  const [cursor, setCursor] = useState<Point>();
  const touch = useMemo(
    () =>
      new TouchInput({
        move: (point) => {
          const now = performance.now();
          if (
            now - lastMove.current >= 12 &&
            session.send(ActionKind.Move, {
              x: Math.round(point.x),
              y: Math.round(point.y),
            })
          )
            lastMove.current = now;
          setCursor(point);
        },
        button: (point, code, down) =>
          session.send(ActionKind.Button, {
            x: Math.round(point.x),
            y: Math.round(point.y),
            code,
            down,
          }),
        scroll: (delta) =>
          session.send(ActionKind.Wheel, {
            x: clamp(delta.x + 1200, 2400) - 1200,
            y: clamp(delta.y + 1200, 2400) - 1200,
          }),
        view: setView,
        release: () => {
          session.send(ActionKind.Release);
        },
      }),
    [session],
  );

  useEffect(() => {
    const player = video.current;
    if (!player) return;
    const dimensions = () =>
      setDecoded({ width: player.videoWidth, height: player.videoHeight });
    player.addEventListener("loadedmetadata", dimensions);
    player.addEventListener("resize", dimensions);
    player.srcObject = session.stream;
    dimensions();
    const disconnect = () => {
      held.current.clear();
      pressed.current.clear();
      touch.cancel();
      setDrag(false);
    };
    const hidden = () => {
      if (document.hidden) disconnect();
    };
    window.addEventListener("blur", disconnect);
    window.addEventListener("pagehide", disconnect);
    window.addEventListener("nexdesk:suspend", disconnect);
    document.addEventListener("visibilitychange", hidden);
    return () => {
      disconnect();
      player.removeEventListener("loadedmetadata", dimensions);
      player.removeEventListener("resize", dimensions);
      player.srcObject = null;
      window.removeEventListener("blur", disconnect);
      window.removeEventListener("pagehide", disconnect);
      window.removeEventListener("nexdesk:suspend", disconnect);
      document.removeEventListener("visibilitychange", hidden);
    };
  }, [session, touch]);

  useEffect(() => {
    touch.cancel();
    setDrag(false);
  }, [state?.generation, canInput, touch]);

  useEffect(() => {
    const resolution = window.matchMedia(`(resolution: ${ratio}dppx)`);
    const update = () => setRatio(window.devicePixelRatio || 1);
    resolution.addEventListener("change", update);
    window.addEventListener("resize", update);
    update();
    return () => {
      resolution.removeEventListener("change", update);
      window.removeEventListener("resize", update);
    };
  }, [ratio]);

  useEffect(() => {
    const element = viewport.current;
    if (!element || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => {
      setViewportSize({ x: element.clientWidth, y: element.clientHeight });
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  const fit =
    decoded.width && decoded.height
      ? Math.min(
          viewportSize.x / decoded.width,
          viewportSize.y / decoded.height,
        )
      : 0;
  const factor = scale === "fit" ? fit : (scale === "double" ? 2 : 1) / ratio;
  const drawnWidth = decoded.width * factor;
  const drawnHeight = decoded.height * factor;
  const centered = (available: number, drawn: number) =>
    Math.round(((available - drawn) * ratio) / 2) / ratio;
  useEffect(() => {
    if (mobile) touch.resize(viewportSize, { x: drawnWidth, y: drawnHeight });
  }, [mobile, viewportSize, drawnWidth, drawnHeight, touch]);
  useEffect(() => {
    touch.resetZoom();
  }, [scale, touch, state?.generation]);

  useEffect(() => {
    const player = video.current;
    if (!player) return;
    player.muted = volume === 0;
    player.volume = volume;
    if (volume > 0)
      void player.play().then(
        () => setSoundIssue(false),
        () => setSoundIssue(true),
      );
    const background = () => player.pause();
    const foreground = () => {
      void player.play().catch(() => setSoundIssue(volume > 0));
    };
    window.addEventListener("nexdesk:suspend", background);
    window.addEventListener("nexdesk:resume", foreground);
    return () => {
      window.removeEventListener("nexdesk:suspend", background);
      window.removeEventListener("nexdesk:resume", foreground);
    };
  }, [volume]);

  useEffect(() => {
    const element = area.current;
    if (!element || !canInput) return;
    const wheel = (event: WheelEvent) => {
      if (document.activeElement !== element || event.ctrlKey) return;
      event.preventDefault();
      const factor =
        event.deltaMode === 1 ? 90 : event.deltaMode === 2 ? 480 : 1.8;
      const bound = (value: number) => clamp(value + 1200, 2400) - 1200;
      session.send(ActionKind.Wheel, {
        x: bound(-event.deltaX * factor),
        y: bound(-event.deltaY * factor),
      });
    };
    element.addEventListener("wheel", wheel, { passive: false });
    return () => element.removeEventListener("wheel", wheel);
  }, [canInput, session]);

  const release = () => {
    pressed.current.clear();
    held.current.clear();
    touch.cancel();
    setDrag(false);
  };

  const point = (position: Point) => {
    const player = video.current;
    if (!player || !player.videoWidth || !player.videoHeight) return undefined;
    const bounds = videoBounds(player.getBoundingClientRect(), {
      width: player.videoWidth,
      height: player.videoHeight,
    });
    if (!bounds) return undefined;
    const x = (position.x - bounds.x) / bounds.width;
    const y = (position.y - bounds.y) / bounds.height;
    if (held.current.size === 0 && (x < 0 || x > 1 || y < 0 || y > 1))
      return undefined;
    return { x: clamp(x * 65535, 65535), y: clamp(y * 65535, 65535) };
  };

  const touches = (event: PointerEvent) =>
    mobile && event.pointerType !== "mouse";
  const position = (event: PointerEvent) => ({
    x: event.clientX,
    y: event.clientY,
  });
  const click = (event: PointerEvent<HTMLDivElement>, down: boolean) => {
    if (touches(event)) {
      if (down) {
        if (touch.down(event.pointerId, position(event), event.timeStamp)) {
          event.currentTarget.focus({ preventScroll: true });
          event.currentTarget.setPointerCapture(event.pointerId);
        }
      } else {
        touch.up(
          event.pointerId,
          position(event),
          event.timeStamp,
          mode,
          point,
          Boolean(canInput),
        );
      }
      event.preventDefault();
      return;
    }
    if (!canInput || !event.isPrimary) return;
    const xy = point(position(event));
    const code = buttons[event.button];
    if (xy === undefined || code === undefined) return;
    if (down) {
      event.currentTarget.focus({ preventScroll: true });
      event.currentTarget.setPointerCapture(event.pointerId);
    }
    if (session.send(ActionKind.Button, { ...xy, code, down })) {
      if (down) held.current.add(code);
      else held.current.delete(code);
    }
    event.preventDefault();
  };

  function key(event: KeyboardEvent, down: boolean) {
    if (!canInput || event.nativeEvent.isComposing) return;
    if (event.code === "Escape" && event.shiftKey) {
      release();
      area.current?.blur();
      event.preventDefault();
      return;
    }
    const code = hid(event.code);
    if (code === undefined) return;
    event.preventDefault();
    if (down && pressed.current.has(code)) return;
    if (session.send(ActionKind.Key, { code, down })) {
      if (down) pressed.current.add(code);
      else pressed.current.delete(code);
    }
  }

  const marker = (() => {
    if (!mobile || !cursor || mode !== "trackpad" || !canInput) return;
    const player = video.current;
    const parent = area.current?.getBoundingClientRect();
    if (!player || !parent) return;
    const bounds = videoBounds(player.getBoundingClientRect(), {
      width: player.videoWidth,
      height: player.videoHeight,
    });
    if (!bounds) return;
    return {
      left: bounds.x - parent.left + (bounds.width * cursor.x) / 65535,
      top: bounds.y - parent.top + (bounds.height * cursor.y) / 65535,
    };
  })();
  return (
    <>
      {controls &&
        createPortal(
          <>
            {mobile && (canInput || view.zoom > 1) && (
              <div className="bar touch-controls">
                {canInput && (
                  <>
                    <button
                      aria-pressed={mode === "trackpad"}
                      onClick={() => {
                        release();
                        setMode("trackpad");
                        onToolAction();
                      }}
                    >
                      触控板
                    </button>
                    <button
                      aria-pressed={mode === "direct"}
                      onClick={() => {
                        release();
                        setMode("direct");
                        onToolAction();
                      }}
                    >
                      直接触控
                    </button>
                    <button
                      aria-pressed={drag}
                      onPointerDown={(event) => event.preventDefault()}
                      onClick={() => {
                        setDrag(touch.toggleDrag());
                        onToolAction();
                      }}
                    >
                      {drag ? "松开左键" : "按住左键"}
                    </button>
                  </>
                )}
                {view.zoom > 1 && (
                  <button
                    onClick={() => {
                      touch.resetZoom();
                      onToolAction();
                    }}
                  >
                    还原缩放
                  </button>
                )}
              </div>
            )}
            {soundIssue && Boolean(session.capabilities & Capability.Audio) && (
              <button
                onClick={() => {
                  const player = video.current;
                  if (!player) return;
                  player.muted = false;
                  player.volume = volume;
                  void player.play().then(
                    () => setSoundIssue(false),
                    () => setSoundIssue(true),
                  );
                }}
              >
                允许声音播放
              </button>
            )}
          </>,
          controls,
        )}
      <div className="viewport" ref={viewport}>
        <div
          ref={area}
          className={mobile ? "surface touch-surface" : "surface"}
          tabIndex={0}
          aria-label="远程桌面画面，Shift Escape 释放键鼠焦点"
          style={
            scale === "fit" || mobile || !decoded.width
              ? undefined
              : {
                  width: drawnWidth,
                  height: drawnHeight,
                  marginLeft: Math.max(0, centered(viewportSize.x, drawnWidth)),
                  marginTop: Math.max(0, centered(viewportSize.y, drawnHeight)),
                }
          }
          onBlur={release}
          onKeyDown={(e) => key(e, true)}
          onKeyUp={(e) => key(e, false)}
          onContextMenu={(event) => {
            if (canInput) event.preventDefault();
          }}
          onPointerDown={(e) => click(e, true)}
          onPointerUp={(e) => click(e, false)}
          onPointerCancel={release}
          onLostPointerCapture={(event) => {
            if (held.current.size || touch.ownsPointer(event.pointerId))
              release();
          }}
          onPointerMove={(event) => {
            if (touches(event)) {
              touch.move(
                event.pointerId,
                position(event),
                {
                  x: event.currentTarget.clientWidth,
                  y: event.currentTarget.clientHeight,
                },
                mode,
                point,
                Boolean(canInput),
              );
              return;
            }
            if (
              !canInput ||
              !event.isPrimary ||
              document.activeElement !== area.current
            )
              return;
            const now = performance.now();
            if (now - lastMove.current < 12) return;
            const xy = point(position(event));
            if (xy && session.send(ActionKind.Move, xy)) lastMove.current = now;
          }}
        >
          <video
            ref={video}
            autoPlay
            playsInline
            muted
            style={
              mobile && drawnWidth > 0 && drawnHeight > 0
                ? {
                    position: "absolute",
                    left: centered(viewportSize.x, drawnWidth),
                    top: centered(viewportSize.y, drawnHeight),
                    width: drawnWidth,
                    height: drawnHeight,
                    transform: `translate(${view.x}px, ${view.y}px) scale(${view.zoom})`,
                    imageRendering:
                      scale === "double" ? "pixelated" : undefined,
                  }
                : {
                    imageRendering:
                      scale === "double" ? "pixelated" : undefined,
                  }
            }
          />
          {marker && (
            <span className="touch-cursor" aria-hidden="true" style={marker} />
          )}
        </div>
      </div>
    </>
  );
}

export const chords: Record<string, string[]> = {
  "Ctrl + Alt + Delete": ["ControlLeft", "AltLeft", "Delete"],
  "Alt + Tab": ["AltLeft", "Tab"],
  "Ctrl + Esc": ["ControlLeft", "Escape"],
  "Win / Command": ["MetaLeft"],
  "Ctrl + Shift + Esc": ["ControlLeft", "ShiftLeft", "Escape"],
};

export function sendChord(session: RemoteSession, name: string) {
  const keys = chords[name]?.map(hid) ?? [];
  session.send(ActionKind.Release);
  for (const code of keys)
    if (code !== undefined) session.send(ActionKind.Key, { code, down: true });
  for (const code of keys.reverse())
    if (code !== undefined) session.send(ActionKind.Key, { code, down: false });
}
