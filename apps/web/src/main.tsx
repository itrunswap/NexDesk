import { StrictMode, useEffect, useRef, useState } from "react";
import type { FormEvent } from "react";
import { createRoot } from "react-dom/client";
import {
  ActionKind,
  Capability,
  RemoteSession,
  forgetIdentity,
  type ConnectionOptions,
  type DesktopState,
  type DisplayMode,
  type ResumeGrant,
} from "./protocol";
import { Files } from "./files";
import {
  loadConnectionPermissions,
  PermissionsDialog,
  storeConnectionPermissions,
} from "./connection-permissions";
import { Surface, chords, sendChord } from "./surface";
import "./style.css";

const embedded =
  new URLSearchParams(location.search).get("client") === "mobile";
const mobile = embedded || matchMedia("(any-pointer: coarse)").matches;

type Target = Pick<ConnectionOptions, "device" | "input" | "capabilities">;

function clearCode(code: { current: Uint8Array | undefined }) {
  code.current?.fill(0);
  code.current = undefined;
}

function errorText(error: unknown) {
  if (error === "peer identity did not match")
    return "设备指纹已变化，请重新独立核对。";
  if (error === "pairing rejected") return "配对失败，请核对连接密码。";
  if (error instanceof Error) return error.message;
  return typeof error === "string" ? error : "操作失败。";
}

function Screen() {
  const [device, setDevice] = useState("");
  const [permissions, setPermissions] = useState(loadConnectionPermissions);
  const [editingPermissions, setEditingPermissions] = useState(false);
  const [status, setStatus] = useState("");
  const [connecting, setConnecting] = useState(false);
  const [session, setSession] = useState<RemoteSession>();
  const [desktop, setDesktop] = useState<DesktopState>();
  const [fingerprint, setFingerprint] = useState("");
  const secret = useRef<HTMLInputElement>(null);
  const preferencesButton = useRef<HTMLButtonElement>(null);
  const active = useRef<RemoteSession | undefined>(undefined);
  const attempt = useRef<AbortController | undefined>(undefined);
  const trust = useRef<((confirmed: boolean) => void) | undefined>(undefined);
  const savedCode = useRef<Uint8Array | undefined>(undefined);
  const foreground = useRef(!document.hidden);

  useEffect(() => {
    const pause = () => {
      foreground.current = false;
      active.current?.setForeground(false);
    };
    const resume = () => {
      foreground.current = !document.hidden;
      active.current?.setForeground(foreground.current);
    };
    const visibility = () => {
      if (document.hidden) pause();
      else resume();
    };
    window.addEventListener("nexdesk:suspend", pause);
    window.addEventListener("nexdesk:resume", resume);
    document.addEventListener("visibilitychange", visibility);
    return () => {
      const current = attempt.current;
      attempt.current = undefined;
      current?.abort();
      clearCode(savedCode);
      trust.current?.(false);
      active.current?.close();
      window.removeEventListener("nexdesk:suspend", pause);
      window.removeEventListener("nexdesk:resume", resume);
      document.removeEventListener("visibilitychange", visibility);
    };
  }, []);

  const decide = (confirmed: boolean) => {
    trust.current?.(confirmed);
    trust.current = undefined;
    setFingerprint("");
  };

  const cancel = () => {
    const previous = attempt.current;
    attempt.current = undefined;
    previous?.abort();
    clearCode(savedCode);
    decide(false);
    active.current?.close();
    active.current = undefined;
    setSession(undefined);
    setConnecting(false);
    setDesktop(undefined);
    setStatus("会话已结束。");
  };

  function finish(owner: AbortController, reason: string) {
    if (attempt.current !== owner) return;
    attempt.current = undefined;
    active.current = undefined;
    clearCode(savedCode);
    setSession(undefined);
    setDesktop(undefined);
    setConnecting(false);
    setStatus(reason);
  }

  function install(owner: AbortController, connected: RemoteSession) {
    if (attempt.current !== owner) {
      connected.close();
      return;
    }
    active.current = connected;
    connected.setForeground(foreground.current);
    setSession(connected);
    setConnecting(false);
    setStatus("");
  }

  function open(
    owner: AbortController,
    target: Target,
    code: Uint8Array,
    signal = owner.signal,
    resume?: ResumeGrant,
  ) {
    return RemoteSession.connect({
      ...target,
      code,
      signal,
      resume,
      trust: (identity) =>
        new Promise((resolve) => {
          if (signal.aborted || resume) resolve(false);
          else {
            trust.current = resolve;
            setFingerprint(identity);
          }
        }),
      status: setStatus,
      desktop: setDesktop,
      closed: (reason, token) => {
        if (attempt.current !== owner) return;
        active.current = undefined;
        setSession(undefined);
        setDesktop(undefined);
        if (token && savedCode.current) {
          void restore(owner, target.device, token);
        } else {
          finish(owner, reason || "会话已结束。");
        }
      },
    });
  }

  async function restore(
    owner: AbortController,
    device: string,
    resume: ResumeGrant,
  ) {
    setConnecting(true);
    setStatus("网络中断，正在恢复原会话。");
    const bounded = new AbortController();
    const cancel = () => bounded.abort();
    owner.signal.addEventListener("abort", cancel, { once: true });
    const timer = window.setTimeout(cancel, 15_000);
    try {
      let attempts = 0;
      while (!bounded.signal.aborted && attempts < 3) {
        await new Promise((resolve) => window.setTimeout(resolve, 800));
        if (bounded.signal.aborted || !savedCode.current) break;
        if (!foreground.current || !navigator.onLine) continue;
        attempts++;
        try {
          const connected = await open(
            owner,
            {
              device,
              input: resume.input,
              capabilities: resume.capabilities,
            },
            savedCode.current.slice(),
            bounded.signal,
            resume,
          );
          install(owner, connected);
          return;
        } catch (error) {
          if (bounded.signal.aborted) break;
          const retryable =
            error instanceof TypeError ||
            ["被控端正忙，请稍后重试。", "请求过于频繁，请稍后重试。"].includes(
              errorText(error),
            );
          if (!retryable) break;
        }
      }
      finish(owner, "原会话无法恢复，请重新输入密码连接。");
    } finally {
      clearTimeout(timer);
      owner.signal.removeEventListener("abort", cancel);
    }
  }

  async function connect(event: FormEvent) {
    event.preventDefault();
    if (editingPermissions || attempt.current || !secret.current) return;
    if (!window.isSecureContext || !window.RTCPeerConnection) {
      setStatus("请通过可信 HTTPS 和支持 WebRTC 的浏览器打开页面。");
      return;
    }
    const code = new TextEncoder().encode(secret.current.value);
    secret.current.value = "";
    const controller = new AbortController();
    if (mobile) savedCode.current = code.slice();
    attempt.current = controller;
    setConnecting(true);
    setStatus("验证被控设备");
    try {
      const connected = await open(
        controller,
        {
          device,
          ...permissions,
        },
        code,
      );
      install(controller, connected);
    } catch (error) {
      finish(controller, errorText(error));
    }
  }

  function closePreferences() {
    setEditingPermissions(false);
    requestAnimationFrame(() => preferencesButton.current?.focus());
  }

  return (
    <main
      className={[
        session ? "shell shell-session" : "shell",
        mobile && "mobile-client",
        embedded && "embedded-client",
      ]
        .filter(Boolean)
        .join(" ")}
    >
      {!embedded && !session && (
        <header className="brandbar">
          <h1>
            <span className="desk-mark" aria-hidden="true" />
            NexDesk
          </h1>
          <span className="edition">Web 控制台</span>
        </header>
      )}
      {session ? (
        <Workspace session={session} state={desktop} cancel={cancel} />
      ) : (
        <div className="connection-grid">
          {!embedded && (
            <aside
              className="connection-info"
              aria-labelledby="service-heading"
            >
              <h2 id="service-heading">连接服务</h2>
              <div className="service-address">
                <span className="service-label">当前服务器</span>
                <strong>{location.host}</strong>
              </div>
              <p>
                在被控电脑打开
                NexDesk，确认它已连接同一服务，然后使用该电脑显示的设备 ID
                和连接密码。
              </p>
              <p>被控电脑授权后，会在此浏览器中打开远程桌面。</p>
            </aside>
          )}
          <form
            className="connect"
            inert={editingPermissions}
            onSubmit={(event) => void connect(event)}
          >
            <div className="connect-heading">
              <h2>连接远程电脑</h2>
              <button
                type="button"
                ref={preferencesButton}
                disabled={connecting}
                onClick={() => setEditingPermissions(true)}
              >
                连接偏好
              </button>
            </div>
            <label>
              设备 ID{" "}
              <input
                name="device"
                inputMode="numeric"
                pattern="[0-9]{9}"
                maxLength={9}
                placeholder="输入九位设备 ID"
                value={device}
                onChange={(event) => setDevice(event.target.value)}
                required
                disabled={connecting}
              />
            </label>
            <label>
              连接密码{" "}
              <input
                name="code"
                type="password"
                autoComplete="off"
                ref={secret}
                pattern="[A-Za-z0-9]{6}"
                maxLength={6}
                placeholder="输入六位连接密码"
                required
                disabled={connecting}
              />
            </label>
            <div className="bar connect-actions">
              {connecting ? (
                <button type="button" onClick={cancel}>
                  取消连接
                </button>
              ) : (
                <button className="primary" type="submit">
                  连接
                </button>
              )}
            </div>
            {status && <output role="status">{status}</output>}
          </form>
        </div>
      )}
      {editingPermissions && (
        <PermissionsDialog
          value={permissions}
          onClose={closePreferences}
          onSave={(next) => {
            storeConnectionPermissions(next);
            setPermissions(next);
            closePreferences();
          }}
          onForget={
            /^[0-9]{9}$/u.test(device)
              ? () => {
                  try {
                    forgetIdentity(device);
                    setStatus("已清除该设备的信任记录。");
                  } catch (error) {
                    setStatus(errorText(error));
                  }
                  closePreferences();
                }
              : undefined
          }
        />
      )}
      {fingerprint && (
        <div className="scrim">
          <section
            role="dialog"
            aria-modal="true"
            aria-labelledby="trust-heading"
            className="dialog"
          >
            <h2 id="trust-heading">核对设备指纹</h2>
            <p>通过可信渠道与被控端显示的完整指纹核对。</p>
            <code>{fingerprint}</code>
            <div className="bar">
              <button onClick={() => decide(false)}>取消</button>
              <button className="primary" onClick={() => decide(true)}>
                指纹一致
              </button>
            </div>
          </section>
        </div>
      )}
    </main>
  );
}

function Workspace({
  session,
  state,
  cancel,
}: {
  session: RemoteSession;
  state: DesktopState | undefined;
  cancel: () => void;
}) {
  const [scale, setScale] = useState<"fit" | "actual" | "double">("fit");
  const [volume, setVolume] = useState(0);
  const [preset, setPreset] = useState("auto");
  const [fps, setFps] = useState(30);
  const [menuOpen, setMenuOpen] = useState(false);
  const [surfaceControls, setSurfaceControls] = useState<HTMLDivElement | null>(
    null,
  );
  const [filePanel, showFiles] = useState(false);
  const [keyboard, showKeyboard] = useState(false);
  const [status, setStatus] = useState("");
  const [network, setNetwork] = useState("");
  const [text, setText] = useState("");
  const [modes, setModes] = useState<DisplayMode[]>([]);
  const [rememberMode, setRememberMode] = useState(true);
  const [preview, setPreview] = useState<{
    token: number;
    expires_ms: number;
  }>();
  const view = useRef<HTMLDivElement>(null);
  const menuButton = useRef<HTMLButtonElement>(null);
  const grant = (bit: number) => Boolean(session.capabilities & bit);
  const remembered = state
    ? session.rememberedMode(state.selected, modes)
    : undefined;

  function closeMenu() {
    setMenuOpen(false);
    menuButton.current?.focus({ preventScroll: true });
  }

  function toggleMenu() {
    if (menuOpen) {
      closeMenu();
    } else {
      session.send(ActionKind.Release);
      menuButton.current?.focus({ preventScroll: true });
      setMenuOpen(true);
    }
  }

  useEffect(() => {
    let closed = false;
    const refresh = () => {
      void session
        .network()
        .then((line) => {
          if (!closed) setNetwork(line);
        })
        .catch(() => undefined);
    };
    refresh();
    const timer = setInterval(refresh, 2_000);
    return () => {
      closed = true;
      clearInterval(timer);
    };
  }, [session]);
  useEffect(() => {
    if (!preview) return;
    const timer = setTimeout(() => setPreview(undefined), preview.expires_ms);
    return () => clearTimeout(timer);
  }, [preview]);

  const run = (operation: () => Promise<unknown>, success = "") => {
    void operation().then(
      () => setStatus(success),
      (error: unknown) => setStatus(errorText(error)),
    );
  };
  const applyQuality = (nextPreset: string, nextFps: number) => {
    run(async () => {
      await session.collaboration.request({
        kind: "quality",
        quality: { preset: nextPreset, fps: nextFps },
      });
      setPreset(nextPreset);
      setFps(nextFps);
    });
  };
  const sendText = (event: FormEvent) => {
    event.preventDefault();
    if (!state?.can_input || !text.length) return;
    const direct = grant(Capability.Text);
    run(
      async () => {
        if (new TextEncoder().encode(text).length > 4096)
          throw new Error("输入文本超过 4 KiB。");
        await session.collaboration.request(
          direct
            ? { kind: "text", generation: state.generation, text }
            : { kind: "clipboard_write", text },
        );
        setText("");
      },
      direct ? "" : "文本已写入远端剪贴板，可在远端粘贴。",
    );
  };
  const textInput = (
    <form className="bar input-method" onSubmit={sendText}>
      <label>
        输入法文本{" "}
        <input
          value={text}
          onChange={(event) => setText(event.target.value)}
          autoComplete="off"
          autoCapitalize="off"
          spellCheck={false}
        />
      </label>
      <button type="submit">
        {grant(Capability.Text) ? "发送" : "写入远端剪贴板"}
      </button>
    </form>
  );
  const clipboard = async (read: boolean) => {
    if (!navigator.clipboard) throw new Error("此浏览器未开放本地剪贴板。");
    if (read) {
      // Start the write inside the user gesture, including on Safari.
      const reply = session.collaboration.request<{ text: string }>({
        kind: "clipboard_read",
      });
      if (typeof ClipboardItem !== "undefined" && navigator.clipboard.write) {
        await navigator.clipboard.write([
          new ClipboardItem({
            "text/plain": reply.then(
              ({ value }) => new Blob([value.text], { type: "text/plain" }),
            ),
          }),
        ]);
      } else {
        await navigator.clipboard.writeText((await reply).value.text);
      }
    } else {
      const local = await navigator.clipboard.readText();
      if (new TextEncoder().encode(local).length > 4096)
        throw new Error("剪贴板文本超过 4 KiB。");
      await session.collaboration.request({
        kind: "clipboard_write",
        text: local,
      });
    }
  };

  return (
    <div className="workspace" ref={view}>
      <div className="workspace-stage">
        <Surface
          session={session}
          state={state}
          scale={scale}
          volume={volume}
          mobile={mobile}
          controls={surfaceControls}
          onToolAction={closeMenu}
        />
        {(status || state?.notice) && (
          <output role="status" className="session-notice">
            {status || state?.notice}
          </output>
        )}
      </div>
      {menuOpen && (
        <div
          className="session-shield"
          aria-hidden="true"
          onPointerDown={closeMenu}
        />
      )}
      <div
        className="session-tools"
        onKeyDown={(event) => {
          if (menuOpen && event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            closeMenu();
          }
        }}
      >
        <button
          className="session-trigger"
          type="button"
          ref={menuButton}
          title={menuOpen ? "收起会话操作" : "会话操作"}
          aria-label={menuOpen ? "收起会话操作" : "会话操作"}
          aria-expanded={menuOpen}
          aria-controls="session-menu"
          onClick={toggleMenu}
        >
          <span aria-hidden="true">{menuOpen ? "×" : "⋯"}</span>
        </button>
        <section
          id="session-menu"
          className="session-menu"
          aria-label="会话操作"
          hidden={!menuOpen}
        >
          <div className="session-heading">
            <div>
              <h2>会话操作</h2>
              <div className="state">
                {state
                  ? state.can_input
                    ? "键鼠控制"
                    : "只读观看"
                  : "正在连接媒体"}
                {network && ` · ${network}`}
              </div>
            </div>
            <button className="end" type="button" onClick={cancel}>
              断开
            </button>
          </div>
          <div className="bar">
            <label>
              显示器{" "}
              <select
                value={state?.selected ?? ""}
                disabled={!state || Boolean(preview)}
                onChange={(event) => {
                  setModes([]);
                  session.send(ActionKind.Release);
                  session.send(ActionKind.Select, {
                    display: event.target.value,
                  });
                }}
              >
                {state?.displays.map(({ id, label }) => (
                  <option key={id} value={id}>
                    {label}
                  </option>
                ))}
              </select>
            </label>
            {!mobile && (
              <>
                <label>
                  缩放{" "}
                  <select
                    value={scale}
                    onChange={(event) => {
                      const value = event.target.value;
                      if (
                        value === "fit" ||
                        value === "actual" ||
                        value === "double"
                      )
                        setScale(value);
                    }}
                  >
                    <option value="fit">适配</option>
                    <option value="actual">原始</option>
                    <option value="double">200%</option>
                  </select>
                </label>
                <button
                  onClick={() => {
                    closeMenu();
                    run(async () => {
                      if (document.fullscreenElement)
                        await document.exitFullscreen();
                      else await view.current?.requestFullscreen();
                    });
                  }}
                >
                  {document.fullscreenElement ? "退出全屏" : "全屏"}
                </button>
              </>
            )}
            {mobile && state?.can_input && (
              <button
                aria-pressed={keyboard}
                onClick={() => {
                  showKeyboard(!keyboard);
                  closeMenu();
                }}
              >
                {keyboard ? "收起键盘" : "键盘"}
              </button>
            )}
            {(grant(Capability.FileRead) || grant(Capability.FileWrite)) && (
              <button
                onClick={() => {
                  showFiles(!filePanel);
                  closeMenu();
                }}
              >
                {filePanel ? "收起文件" : "文件"}
              </button>
            )}
          </div>
          <div ref={setSurfaceControls} className="session-surface-controls" />
          <section className="session-options" aria-label="会话设置">
            <div className="bar">
              <label>
                画质{" "}
                <select
                  value={preset}
                  onChange={(event) => applyQuality(event.target.value, fps)}
                >
                  <option value="auto">自动</option>
                  <option value="clear">清晰</option>
                  <option value="smooth">流畅</option>
                </select>
              </label>
              <label>
                帧率{" "}
                <select
                  value={fps}
                  onChange={(event) =>
                    applyQuality(preset, Number(event.target.value))
                  }
                >
                  {[15, 30, 45, 60].map((rate) => (
                    <option key={rate}>{rate}</option>
                  ))}
                </select>
              </label>
              {grant(Capability.Audio) && (
                <>
                  <button onClick={() => setVolume(volume === 0 ? 1 : 0)}>
                    {volume === 0 ? "解除静音" : "静音"}
                  </button>
                  <label>
                    音量{" "}
                    <input
                      type="range"
                      min={0}
                      max={1}
                      step={0.05}
                      value={volume}
                      onChange={(event) =>
                        setVolume(Number(event.target.value))
                      }
                    />
                  </label>
                </>
              )}
              {state?.can_input && (
                <label>
                  组合键{" "}
                  <select
                    value=""
                    onChange={(event) => sendChord(session, event.target.value)}
                  >
                    <option value="">选择</option>
                    {Object.keys(chords).map((chord) => (
                      <option key={chord}>{chord}</option>
                    ))}
                  </select>
                </label>
              )}
              {grant(Capability.Clipboard) && (
                <>
                  <button
                    onClick={() =>
                      run(() => clipboard(true), "已复制远端文本。")
                    }
                  >
                    读远端剪贴板
                  </button>
                  <button
                    onClick={() =>
                      run(() => clipboard(false), "已写入远端剪贴板。")
                    }
                  >
                    写远端剪贴板
                  </button>
                </>
              )}
              {grant(Capability.Display) && state?.selected && (
                <button
                  disabled={Boolean(preview)}
                  onClick={() =>
                    run(async () => {
                      const { value } = await session.collaboration.request<{
                        choices: DisplayMode[];
                      }>({
                        kind: "modes",
                        display: state.selected,
                      });
                      setModes(value.choices);
                    })
                  }
                >
                  读取分辨率
                </button>
              )}
              {modes.length > 0 && state?.selected && (
                <label>
                  分辨率{" "}
                  <select
                    value=""
                    disabled={Boolean(preview)}
                    onChange={(event) => {
                      const mode = modes[Number(event.target.value)];
                      if (!event.target.value || !mode) return;
                      run(async () => {
                        const { value } = await session.collaboration.request<{
                          token: number;
                          expires_ms: number;
                        }>({
                          kind: "apply",
                          display: state.selected,
                          mode,
                        });
                        setPreview(value);
                        setMenuOpen(true);
                      });
                    }}
                  >
                    <option value="">选择</option>
                    {modes.map((mode, index) => (
                      <option key={index} value={index}>
                        {mode.width} × {mode.height} ·{" "}
                        {mode.refresh_millihz / 1000} Hz
                        {mode === remembered && " · 上次确认"}
                      </option>
                    ))}
                  </select>
                </label>
              )}
            </div>
            {preview && (
              <div className="bar">
                <span>请确认远端画面可用</span>
                <label className="check">
                  <input
                    type="checkbox"
                    checked={rememberMode}
                    onChange={(event) => setRememberMode(event.target.checked)}
                  />
                  记住确认的分辨率
                </label>
                {(["confirm", "rollback"] as const).map((kind) => (
                  <button
                    key={kind}
                    onClick={() =>
                      run(async () => {
                        const { value } = await session.collaboration.request<{
                          display: string;
                          mode: DisplayMode;
                        }>({
                          kind,
                          token: preview.token,
                        });
                        setPreview(undefined);
                        if (kind === "confirm" && rememberMode)
                          session.rememberMode(value.display, value.mode);
                      })
                    }
                  >
                    {kind === "confirm" ? "保留" : "恢复"}
                  </button>
                ))}
              </div>
            )}
            {!mobile && grant(Capability.Text) && state?.can_input && textInput}
          </section>
        </section>
      </div>
      {(filePanel || (mobile && keyboard && state?.can_input)) && (
        <div className="workspace-aux">
          {mobile && keyboard && state?.can_input && (
            <section className="settings touch-keyboard">
              <div className="bar">
                {[
                  ["Esc", 0x70029],
                  ["Tab", 0x7002b],
                  ["退格", 0x7002a],
                  ["回车", 0x70028],
                  ["←", 0x70050],
                  ["↑", 0x70052],
                  ["↓", 0x70051],
                  ["→", 0x7004f],
                ].map(([label, code]) => (
                  <button
                    type="button"
                    key={label}
                    onClick={() => {
                      session.send(ActionKind.Key, {
                        code: Number(code),
                        down: true,
                      });
                      session.send(ActionKind.Key, {
                        code: Number(code),
                        down: false,
                      });
                    }}
                  >
                    {label}
                  </button>
                ))}
              </div>
              {(grant(Capability.Text) || grant(Capability.Clipboard)) &&
                textInput}
            </section>
          )}
          {filePanel && <Files session={session} />}
        </div>
      )}
    </div>
  );
}

const container = document.getElementById("root");
if (container)
  createRoot(container).render(
    <StrictMode>
      <Screen />
    </StrictMode>,
  );
