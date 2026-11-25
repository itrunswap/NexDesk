type BrowserCore = typeof import("../public/core/nexdesk_core.js");

let runtime: Promise<BrowserCore> | undefined;

export function browserCore(): Promise<BrowserCore> {
  runtime ??= (async () => {
    const entry = "/core/nexdesk_core.js";
    const core = (await import(/* @vite-ignore */ entry)) as BrowserCore;
    await core.default();
    return core;
  })().catch((error: unknown) => {
    runtime = undefined;
    throw error;
  });
  return runtime;
}

export type DisplayMode = {
  width: number;
  height: number;
  refresh_millihz: number;
};

export type DesktopState = {
  generation: number;
  can_input: boolean;
  selected: string;
  displays: { id: string; label: string }[];
  width: number;
  height: number;
  notice: string;
};

export const Capability = {
  Audio: 1,
  Clipboard: 2,
  FileRead: 4,
  FileWrite: 8,
  Display: 16,
  Text: 32,
} as const;

export enum ActionKind {
  Move = 1,
  Button,
  Wheel,
  Key,
  Release,
  Select,
  Close,
  Heartbeat,
}

type Action = {
  kind: ActionKind;
  generation: number;
  x: number;
  y: number;
  code: number;
  down: boolean;
  display: string;
};

type Packet<T> = { Ok: T } | { Err: string };
type Reply<T> = { value: T; data: Uint8Array };
type Waiting = {
  accept: (data: Reply<unknown>) => void;
  fail: (reason: Error) => void;
  timer: number;
};

class Channel {
  private pending = new Map<number, Waiting>();
  private serial = 0;

  constructor(
    private readonly channel: RTCDataChannel,
    private readonly core: BrowserCore,
  ) {
    channel.binaryType = "arraybuffer";
    channel.addEventListener("message", this.receive);
    channel.addEventListener("close", this.shutdown);
  }

  private receive = (event: MessageEvent<unknown>) => {
    if (!(event.data instanceof ArrayBuffer)) {
      this.shutdown();
      return;
    }
    try {
      const packet = this.core.decode_collaboration(new Uint8Array(event.data));
      try {
        const serial = packet.id();
        const waiting = this.pending.get(serial);
        if (!waiting) return;
        this.pending.delete(serial);
        window.clearTimeout(waiting.timer);
        const message = JSON.parse(packet.message()) as Packet<unknown>;
        if ("Err" in message) waiting.fail(new Error(message.Err));
        else waiting.accept({ value: message.Ok, data: packet.bytes() });
      } finally {
        packet.free();
      }
    } catch {
      this.shutdown();
    }
  };

  private shutdown = () => {
    for (const waiting of this.pending.values()) {
      window.clearTimeout(waiting.timer);
      waiting.fail(new Error("会话通道已关闭。"));
    }
    this.pending.clear();
    this.channel.close();
  };

  request<T>(message: object, data = new Uint8Array()): Promise<Reply<T>> {
    if (
      this.channel.readyState !== "open" ||
      this.channel.bufferedAmount > 64 * 1024 ||
      this.pending.size >= 16
    ) {
      return Promise.reject(new Error("会话通道暂不可用。"));
    }
    const serial = (this.serial = (this.serial % 0xffffffff) + 1);
    return new Promise((accept, fail) => {
      const packet = this.core.collaboration_packet(
        serial,
        JSON.stringify(message),
        data,
      );
      const timer = window.setTimeout(() => {
        this.pending.delete(serial);
        fail(new Error("远端操作超时。"));
      }, 30_000);
      this.pending.set(serial, {
        accept: (result) => accept(result as Reply<T>),
        fail,
        timer,
      });
      try {
        this.channel.send(packet.slice());
      } catch (error) {
        window.clearTimeout(timer);
        this.pending.delete(serial);
        fail(error);
      }
    });
  }
}

type Route = {
  id: string;
  ice: { servers: RTCIceServer[]; relay_only: boolean };
};

type Answer = {
  sdp: string;
  grant: "view" | "control";
  resume: string;
  remote: { origin: string; device: string };
  capabilities: number;
};

export type ResumeGrant = {
  token: string;
  identity: string;
  input: boolean;
  capabilities: number;
};

export type ConnectionOptions = {
  device: string;
  code: Uint8Array;
  input: boolean;
  capabilities: number;
  resume?: ResumeGrant;
  signal: AbortSignal;
  trust: (identity: string) => Promise<boolean>;
  status: (text: string) => void;
  desktop: (state: DesktopState) => void;
  closed: (reason: string, resume?: ResumeGrant) => void;
};

const responseLimit = 80 * 1024;

async function post<T>(
  url: string,
  body: unknown,
  parent: AbortSignal,
  timeout: number,
): Promise<T> {
  const controller = new AbortController();
  const cancel = () => controller.abort(parent.reason);
  parent.throwIfAborted();
  parent.addEventListener("abort", cancel, { once: true });
  const timer = window.setTimeout(
    () => controller.abort(new Error("信令请求超时。")),
    timeout,
  );
  try {
    const response = await fetch(url, {
      method: "POST",
      cache: "no-store",
      credentials: "omit",
      redirect: "error",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
      signal: controller.signal,
    });
    if (!response.ok) {
      const messages: Record<number, string> = {
        401: "配对失败，请核对连接密码。",
        403: "配对失败，或被控端拒绝了会话。",
        404: "设备离线或设备 ID 不存在。",
        409: "被控端正忙，请稍后重试。",
        410: "协商已过期，请重新连接。",
        429: "请求过于频繁，请稍后重试。",
        503: "服务或设备暂不可用。",
      };
      throw new Error(messages[response.status] ?? "信令处理失败。");
    }
    const reader = response.body?.getReader();
    if (!reader) throw new Error("服务未返回数据。");
    let received = 0;
    let raw = "";
    const decoder = new TextDecoder("utf-8", { fatal: true });
    try {
      for (;;) {
        const chunk = await reader.read();
        if (chunk.done) break;
        received += chunk.value.byteLength;
        if (received > responseLimit) throw new Error("信令响应过大。");
        raw += decoder.decode(chunk.value, { stream: true });
      }
      return JSON.parse(raw + decoder.decode()) as T;
    } finally {
      await reader.cancel();
    }
  } finally {
    clearTimeout(timer);
    parent.removeEventListener("abort", cancel);
  }
}

function gather(peer: RTCPeerConnection, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    const done = () => {
      clearTimeout(timer);
      peer.removeEventListener("icegatheringstatechange", changed);
      signal.removeEventListener("abort", abort);
      if (signal.aborted) reject(signal.reason);
      else resolve();
    };
    const changed = () => {
      if (peer.iceGatheringState === "complete") done();
    };
    const abort = () => done();
    const timer = window.setTimeout(done, 6_000);
    peer.addEventListener("icegatheringstatechange", changed);
    signal.addEventListener("abort", abort, { once: true });
    if (signal.aborted) abort();
    else changed();
  });
}

const iceUrl = /^(stuns?|turns?):[^\/@#\s]{1,250}$/u;
function checkRoute(route: Route) {
  if (
    !/^[A-Za-z0-9_-]{43}$/u.test(route.id) ||
    !Array.isArray(route.ice.servers) ||
    route.ice.servers.length > 8 ||
    typeof route.ice.relay_only !== "boolean" ||
    route.ice.servers.some(
      ({ urls, username, credential }) =>
        !Array.isArray(urls) ||
        urls.length === 0 ||
        urls.length > 4 ||
        urls.some((url) => typeof url !== "string" || !iceUrl.test(url)) ||
        typeof username !== "string" ||
        username.length > 256 ||
        typeof credential !== "string" ||
        credential.length > 256,
    )
  ) {
    throw new Error("目录返回的连接配置无效。");
  }
}

const pin = (device: string) => `nexdesk:trust:v1:${device}`;
function readSaved(key: string) {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

export function forgetIdentity(device: string) {
  localStorage.removeItem(pin(device));
}

export class RemoteSession {
  readonly stream = new MediaStream();
  readonly collaboration: Channel;
  readonly files: Channel;
  private latest?: DesktopState;
  private heartbeat?: number;
  private disconnectTimer?: number;
  private closed = false;
  private installed = false;
  private foreground = true;

  private constructor(
    private readonly peer: RTCPeerConnection,
    private readonly control: RTCDataChannel,
    private readonly core: BrowserCore,
    private readonly options: ConnectionOptions,
    readonly capabilities: number,
    readonly grant: "view" | "control",
    private readonly identity: string,
    private readonly resumeToken: string,
    collaboration: RTCDataChannel,
    files: RTCDataChannel,
    stream: MediaStream,
  ) {
    this.stream = stream;
    this.collaboration = new Channel(collaboration, core);
    this.files = new Channel(files, core);
    control.binaryType = "arraybuffer";
    control.addEventListener("open", () => {
      clearTimeout(this.disconnectTimer);
      this.send(ActionKind.Heartbeat);
      this.heartbeat = window.setInterval(
        () => this.send(ActionKind.Heartbeat),
        750,
      );
    });
    control.addEventListener("message", (event: MessageEvent<unknown>) => {
      try {
        if (!(event.data instanceof ArrayBuffer)) throw new Error();
        const reply = JSON.parse(
          core.decode_control(new Uint8Array(event.data)),
        ) as { state?: DesktopState; ended?: number };
        if (reply.state) {
          this.latest = reply.state;
          options.desktop(reply.state);
        } else
          this.close(reply.ended === 1 ? "被控端已撤销授权。" : "会话已结束。");
      } catch {
        this.close("控制数据无效。");
      }
    });
    control.addEventListener("close", () => this.close("连接已中断。", true));
    peer.addEventListener("connectionstatechange", () => {
      if (peer.connectionState === "failed") this.close("媒体连接中断。", true);
      if (peer.connectionState === "connected")
        clearTimeout(this.disconnectTimer);
      if (peer.connectionState === "disconnected") {
        clearTimeout(this.disconnectTimer);
        this.disconnectTimer = window.setTimeout(
          () => this.close("网络连接中断。", true),
          5_000,
        );
      }
    });
    this.disconnectTimer = window.setTimeout(
      () => this.close("媒体连接超时，请检查 ICE 和 TURN 配置。"),
      14_000,
    );
  }

  static async connect(options: ConnectionOptions): Promise<RemoteSession> {
    const { device, code, capabilities, signal } = options;
    const origin = location.origin;
    const pairing = await (async () => {
      try {
        const core = await browserCore();
        signal.throwIfAborted();
        return { core, context: new core.BrowserPairing(code) };
      } finally {
        code.fill(0);
      }
    })();
    const { core, context } = pairing;
    let peer: RTCPeerConnection | undefined;
    try {
      const route = await post<Route>(
        `/api/v1/devices/${device}/session`,
        { version: 1 },
        signal,
        10_000,
      );
      checkRoute(route);
      const path = `/api/v1/sessions/${route.id}`;
      const challenge = await post<unknown>(
        `${path}/start`,
        JSON.parse(context.request()),
        signal,
        10_000,
      );
      peer = new RTCPeerConnection({
        iceServers: route.ice.servers,
        iceTransportPolicy: route.ice.relay_only ? "relay" : "all",
        bundlePolicy: "max-bundle",
      });
      const stream = new MediaStream();
      peer.addEventListener("track", ({ track }) => stream.addTrack(track));
      peer.addTransceiver("video", { direction: "recvonly" });
      if (capabilities & Capability.Audio) {
        peer.addTransceiver("audio", { direction: "recvonly" });
      }
      const channels = [
        peer.createDataChannel("nexdesk-control", {
          negotiated: true,
          id: 0,
          protocol: "nexdesk.control.v2",
        }),
        peer.createDataChannel("nexdesk-collaboration", {
          negotiated: true,
          id: 2,
          protocol: "nexdesk.collaboration.v1",
        }),
        peer.createDataChannel("nexdesk-files", {
          negotiated: true,
          id: 4,
          protocol: "nexdesk.files.v1",
        }),
      ] as const;
      await peer.setLocalDescription(await peer.createOffer());
      await gather(peer, signal);
      const sdp = peer.localDescription?.sdp;
      if (!sdp?.includes("\na=candidate:"))
        throw new Error("未获得 ICE 地址，请检查本机网络与 TURN。");
      const previous = options.resume?.identity ?? readSaved(pin(device)) ?? "";
      const proof = JSON.parse(
        context.confirm(
          JSON.stringify(challenge),
          previous,
          JSON.stringify({
            sdp,
            request_input: options.input,
            resume: options.resume?.token ?? null,
            capabilities,
            remote: { device, origin },
          }),
        ),
      ) as { identity: string; confirmation: unknown };
      if (!previous && !(await options.trust(proof.identity))) {
        throw new Error("未确认被控设备指纹。");
      }
      options.status(options.resume ? "验证原会话授权" : "等待被控电脑同意");
      const reply = await post<{ answer: string }>(
        `${path}/finish`,
        proof.confirmation,
        signal,
        45_000,
      );
      const answer = JSON.parse(context.answer(reply.answer)) as Answer;
      if (
        answer.remote.origin !== origin ||
        answer.remote.device !== device ||
        !["view", "control"].includes(answer.grant) ||
        (!options.input && answer.grant === "control") ||
        !Number.isSafeInteger(answer.capabilities) ||
        answer.capabilities < 0 ||
        answer.capabilities > 63 ||
        (answer.capabilities & ~capabilities) !== 0 ||
        !/^[A-Za-z0-9_-]{43}$/u.test(answer.resume) ||
        typeof answer.sdp !== "string" ||
        answer.sdp.length > 32 * 1024
      ) {
        throw new Error("被控端返回的授权或协商信息无效。");
      }
      const session = new RemoteSession(
        peer,
        channels[0],
        core,
        options,
        answer.capabilities,
        answer.grant,
        proof.identity,
        answer.resume,
        channels[1],
        channels[2],
        stream,
      );
      try {
        await peer.setRemoteDescription({ type: "answer", sdp: answer.sdp });
        signal.throwIfAborted();
        try {
          localStorage.setItem(pin(device), proof.identity);
        } catch {
          /* Browser policy may block persistence for this origin. */
        }
      } catch (error) {
        session.close();
        throw error;
      }
      session.installed = true;
      peer = undefined;
      return session;
    } finally {
      context.free();
      peer?.close();
    }
  }

  send(kind: ActionKind, fields: Partial<Action> = {}) {
    if (
      this.control.readyState !== "open" ||
      this.control.bufferedAmount > 32 * 1024
    )
      return false;
    if (
      kind >= ActionKind.Move &&
      kind <= ActionKind.Key &&
      (!this.foreground || !this.latest?.can_input || this.grant !== "control")
    )
      return false;
    this.control.send(
      this.core
        .control_packet(
          JSON.stringify({
            kind,
            generation: this.latest?.generation ?? 0,
            x: 0,
            y: 0,
            code: 0,
            down: false,
            display: "",
            ...fields,
          }),
        )
        .slice(),
    );
    return true;
  }

  setForeground(foreground: boolean) {
    if (!foreground && this.foreground) this.send(ActionKind.Release);
    this.foreground = foreground;
    if (foreground) this.send(ActionKind.Heartbeat);
  }

  async network(): Promise<string> {
    const stats = await this.peer.getStats();
    for (const record of stats.values()) {
      if (record.type !== "transport" || !record.selectedCandidatePairId)
        continue;
      const pair = stats.get(record.selectedCandidatePairId);
      if (!pair) continue;
      const candidates = [
        stats.get(pair.localCandidateId),
        stats.get(pair.remoteCandidateId),
      ];
      const relay = candidates.some(
        (entry) => entry?.candidateType === "relay",
      );
      const rtt =
        typeof pair.currentRoundTripTime === "number"
          ? ` · RTT ${Math.round(pair.currentRoundTripTime * 1000)} ms`
          : "";
      return (relay ? "TURN 中继" : "直连") + rtt;
    }
    return "";
  }

  rememberedMode(display: string, choices: DisplayMode[]) {
    const mode = readSaved(this.modeKey(display));
    return choices.find((choice) => JSON.stringify(choice) === mode);
  }

  rememberMode(display: string, mode: DisplayMode) {
    localStorage.setItem(this.modeKey(display), JSON.stringify(mode));
  }

  private modeKey(display: string) {
    return `nexdesk:resolution:v1:${this.options.device}:${this.identity}:${display}`;
  }

  close(reason = "", recover = false) {
    if (this.closed) return;
    this.closed = true;
    this.send(ActionKind.Release);
    if (!recover) this.send(ActionKind.Close);
    clearInterval(this.heartbeat);
    clearTimeout(this.disconnectTimer);
    for (const track of this.stream.getTracks()) track.stop();
    this.peer.close();
    if (this.installed) {
      const resume =
        recover && this.latest
          ? {
              token: this.resumeToken,
              identity: this.identity,
              input: this.grant === "control",
              capabilities: this.capabilities,
            }
          : undefined;
      this.options.closed(reason, resume);
    }
  }
}
