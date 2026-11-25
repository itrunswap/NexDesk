import { spawn, type ChildProcessByStdio } from "node:child_process";
import type { Readable, Writable } from "node:stream";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import {
  decodeKittyPrintable,
  isKeyRelease,
  matchesKey,
  parseKey,
  truncateToWidth,
  type TUI,
  type TuiMouseEvent,
  type TuiMouseEventResult,
} from "@earendil-works/pi-tui";

type Bridge = ChildProcessByStdio<Writable, Readable, null>;

const environment = [
  "PATH",
  "Path",
  "SystemRoot",
  "SystemDrive",
  "HOME",
  "USERPROFILE",
  "APPDATA",
  "LOCALAPPDATA",
  "TEMP",
  "TMP",
  "TMPDIR",
  "SSL_CERT_FILE",
  "SSL_CERT_DIR",
  "DISPLAY",
  "XDG_RUNTIME_DIR",
  "DBUS_SESSION_BUS_ADDRESS",
];
const frameLimit = 220_000;
const validSgr = /\x1b\[(?:0|[34]8;2;\d{1,3};\d{1,3};\d{1,3})m/gu;
const keyNames: Record<string, number> = {
  enter: 0x28,
  return: 0x28,
  escape: 0x29,
  esc: 0x29,
  backspace: 0x2a,
  tab: 0x2b,
  space: 0x2c,
  insert: 0x49,
  home: 0x4a,
  pageup: 0x4b,
  delete: 0x4c,
  end: 0x4d,
  pagedown: 0x4e,
  right: 0x4f,
  left: 0x50,
  down: 0x51,
  up: 0x52,
};
const punctuation = [
  ["-", "_", 0x2d],
  ["=", "+", 0x2e],
  ["[", "{", 0x2f],
  ["]", "}", 0x30],
  ["\\", "|", 0x31],
  [";", ":", 0x33],
  ["'", '"', 0x34],
  ["`", "~", 0x35],
  [",", "<", 0x36],
  [".", ">", 0x37],
  ["/", "?", 0x38],
] as const;

function chord(key: string): number[] {
  const flags = { ctrl: 0xe0, shift: 0xe1, alt: 0xe2, super: 0xe3 } as const;
  const usages = new Set<number>();
  for (;;) {
    const prefix = /^(ctrl|shift|alt|super)\+/u.exec(key)?.[1] as
      | keyof typeof flags
      | undefined;
    if (!prefix) break;
    usages.add(flags[prefix]);
    key = key.slice(prefix.length + 1);
  }
  let usage = keyNames[key.toLowerCase()];
  if (/^[a-z]$/iu.test(key)) {
    usage = key.toLowerCase().charCodeAt(0) - 93;
    if (key !== key.toLowerCase()) usages.add(flags.shift);
  } else if (/^[0-9]$/u.test(key)) {
    usage = 0x1e + ((Number(key) + 9) % 10);
  } else if (/^f(?:[1-9]|1[0-2])$/u.test(key)) {
    usage = 0x39 + Number(key.slice(1));
  } else {
    const symbol = punctuation.find(
      ([base, shift]) => base === key || shift === key,
    );
    if (symbol) {
      usage = symbol[2];
      if (key === symbol[1]) usages.add(flags.shift);
    } else if (key.length === 1 && "!@#$%^&*()".includes(key)) {
      usage = 0x1e + "!@#$%^&*()".indexOf(key);
      usages.add(flags.shift);
    }
  }
  return usage === undefined
    ? []
    : [...usages, usage].map((value) => 0x70000 + value);
}

function launch(origin: string, device: string, control: boolean): Bridge {
  const executable =
    process.env.NEXDESK_CLI ??
    (process.platform === "win32" ? "nexdesk-cli.exe" : "nexdesk-cli");
  const args = ["bridge", origin, device];
  if (control) args.push("--control");
  const env = Object.fromEntries(
    environment.map((name) => [name, process.env[name]]),
  );
  return spawn(executable, args, {
    env,
    stdio: ["pipe", "pipe", "ignore"],
    windowsHide: true,
  });
}

class DesktopComponent {
  private child?: Bridge;
  private code = "";
  private line = "";
  private pending = "";
  private frame: string[] = [];
  private status = "输入六位连接密码";
  private fingerprint = "";
  private mode: "password" | "waiting" | "local" | "remote" = "password";
  private viewport = { cols: 72, rows: 14 };
  private sentSize = "";
  private closed = false;
  private mouseButton = 0;
  private previousPointer = 0;
  private isFocused = false;

  constructor(
    private readonly tui: TUI,
    private readonly origin: string,
    private readonly device: string,
    private readonly control: boolean,
    private readonly done: (result: string) => void,
  ) {}

  get focused() {
    return this.isFocused;
  }
  set focused(value: boolean) {
    if (!value && this.isFocused && this.mode === "remote") this.local();
    this.isFocused = value;
  }

  private start() {
    this.mode = "waiting";
    this.status = "正在连接设备";
    const child = launch(this.origin, this.device, this.control);
    this.child = child;
    child.stdin.on("error", () => this.finish("终端连接进程已关闭。"));
    child.stdout.setEncoding("utf8").on("data", (chunk: string) => {
      this.pending += chunk;
      if (this.pending.length > frameLimit * 2) {
        this.finish("终端进程返回的数据过大。");
        return;
      }
      const lines = this.pending.split("\n");
      this.pending = lines.pop() ?? "";
      for (const line of lines) {
        if (line.length > frameLimit) {
          this.finish("终端进程返回的数据过大。");
          return;
        }
        try {
          this.receive(JSON.parse(line) as Record<string, unknown>);
        } catch {
          this.finish("终端连接协议无效。");
          return;
        }
      }
    });
    child.on("error", () =>
      this.finish("无法启动 NexDesk CLI。请检查本地可执行文件路径。"),
    );
    child.on("exit", (code: number | null) =>
      this.finish(code === 0 ? "会话已结束。" : "会话建立失败或网络中断。"),
    );
    this.tui.requestRender();
  }

  private send(message: object) {
    const input = this.child?.stdin;
    if (input?.writable && input.writableLength < 8192)
      input.write(JSON.stringify(message) + "\n");
  }

  private receive(event: Record<string, unknown>) {
    switch (event.event) {
      case "ready":
        if (event.version !== 1 || !this.code)
          throw new Error("bridge version or state");
        this.send({
          kind: "start",
          version: 1,
          code: this.code,
          ...this.viewport,
        });
        this.code = "";
        break;
      case "identity":
        if (
          typeof event.fingerprint !== "string" ||
          !/^[A-Za-z0-9_-]{43}$/u.test(event.fingerprint)
        )
          throw new Error("identity");
        this.fingerprint = event.fingerprint;
        this.status = "与被控端核对完整指纹后按 Enter";
        break;
      case "waiting":
        this.status = "等待被控端批准";
        break;
      case "connected":
        this.mode = "local";
        this.status = "本地命令模式 · help 查看命令";
        break;
      case "status":
        this.status =
          typeof event.text === "string"
            ? event.text.replace(/[^ -~]/gu, " ")
            : "";
        this.mode = event.remote_mode === true ? "remote" : "local";
        break;
      case "desktop":
        if (event.remote_mode === false && this.mode === "remote")
          this.mode = "local";
        break;
      case "frame":
        if (!Array.isArray(event.lines) || event.lines.length > 30)
          throw new Error("frame");
        this.frame = event.lines.map((line: unknown) => {
          if (
            typeof line !== "string" ||
            !/^[▀]*$/u.test(line.replace(validSgr, ""))
          )
            throw new Error("frame row");
          return line;
        });
        break;
      case "ended":
        this.finish("会话已结束。");
        break;
    }
    this.tui.requestRender();
  }

  private local() {
    this.mode = "local";
    this.line = "";
    this.mouseButton = 0;
    this.send({ kind: "release" });
    this.tui.requestRender();
  }

  render(width: number) {
    const cols = Math.max(1, Math.min(width - 1, 100));
    const rows = Math.max(1, Math.min(this.tui.terminal.rows - 8, 30));
    this.viewport = { cols, rows };
    if (
      `${cols}:${rows}` !== this.sentSize &&
      (this.mode === "local" || this.mode === "remote")
    ) {
      this.sentSize = `${cols}:${rows}`;
      this.send({ kind: "size", cols, rows });
    }
    const image = this.frame.slice(0, rows);
    const footer =
      this.mode === "password"
        ? "> " + "•".repeat(this.code.length)
        : this.mode === "local"
          ? "> " + this.line
          : this.mode === "remote"
            ? "Ctrl+G 本地命令 · Ctrl+Q 退出"
            : this.fingerprint;
    return [`NexDesk · ${this.device}`, this.status, ...image, footer].map(
      (line) => truncateToWidth(line, width),
    );
  }

  handleInput(data: string) {
    if (isKeyRelease(data)) return;
    if (
      matchesKey(data, "ctrl+q") ||
      (this.mode !== "remote" && matchesKey(data, "escape"))
    ) {
      this.finish("已关闭远程桌面。");
      return;
    }
    if (this.mode === "remote") {
      if (matchesKey(data, "ctrl+g")) this.local();
      else {
        const codes = chord(parseKey(data) ?? "");
        if (codes.length) this.send({ kind: "keys", codes });
      }
      return;
    }
    const character = decodeKittyPrintable(data) ?? data;
    if (this.mode === "password") {
      if (matchesKey(data, "enter") && this.code.length === 6) this.start();
      else if (matchesKey(data, "backspace"))
        this.code = this.code.slice(0, -1);
      else if (/^[A-Za-z0-9]$/u.test(character) && this.code.length < 6)
        this.code += character;
    } else if (
      this.mode === "waiting" &&
      this.fingerprint &&
      matchesKey(data, "enter")
    ) {
      this.fingerprint = "";
      this.send({ kind: "trust", accept: true });
    } else if (this.mode === "local") {
      if (matchesKey(data, "enter")) {
        this.send({ kind: "local", text: this.line });
        this.line = "";
      } else if (matchesKey(data, "backspace"))
        this.line = this.line.slice(0, -1);
      else if (/^[ -~]$/u.test(character) && this.line.length < 128)
        this.line += character;
    }
    this.tui.requestRender();
  }

  handleMouse(event: TuiMouseEvent): TuiMouseEventResult | undefined {
    if (
      this.mode !== "remote" ||
      event.type === "click" ||
      event.type === "wheel"
    )
      return;
    const move = event.type === "move" || event.type === "drag";
    const row = event.y - 2;
    if (row < 0 || row >= this.frame.length || event.x < 0) {
      if (event.type === "release" && this.mouseButton) this.local();
      return;
    }
    if (move && Date.now() - this.previousPointer < 24)
      return { handled: true, render: false };
    this.previousPointer = Date.now();
    if (event.type === "press")
      this.mouseButton = { left: 1, right: 2, middle: 3, none: 0 }[
        event.button
      ];
    this.send({
      kind: "pointer",
      col: event.x,
      row,
      button: move ? 0 : this.mouseButton,
      down: event.type === "press",
    });
    if (event.type === "release") this.mouseButton = 0;
    return {
      handled: true,
      capture: event.type === "press",
      focus: event.type === "press",
      render: false,
    };
  }

  invalidate() {}

  dispose() {
    this.code = "";
    this.frame = [];
    this.pending = "";
    const child = this.child;
    this.send({ kind: "close" });
    this.child = undefined;
    child?.stdin.end();
    if (child) {
      setTimeout(() => {
        if (child.exitCode === null) child.kill();
      }, 1000).unref();
    }
  }

  finish(result: string) {
    if (this.closed) return;
    this.closed = true;
    this.dispose();
    this.done(result);
  }
}

export default function nexdesk(pi: ExtensionAPI) {
  let active: DesktopComponent | undefined;
  const dispose = () => {
    active?.dispose();
    active = undefined;
  };
  pi.on("session_shutdown", () => active?.finish("会话已关闭。"));
  pi.registerCommand("nexdesk", {
    description: "通过本地 NexDesk CLI 查看和控制远程桌面",
    handler: async (_args, ctx) => {
      if (!ctx.hasUI || ctx.mode !== "tui" || active) return;
      const origin = await ctx.ui.input(
        "自托管目录地址",
        "https://desk.example.org",
      );
      if (!origin) return;
      const device = await ctx.ui.input("九位设备 ID");
      if (!device || !/^[0-9]{9}$/u.test(device)) return;
      const mode = await ctx.ui.select("申请的权限", ["只读观看", "键鼠控制"]);
      if (!mode) return;
      try {
        const ended = await ctx.ui.custom<string>(
          (tui, _theme, _keys, done) => {
            active = new DesktopComponent(
              tui,
              origin,
              device,
              mode === "键鼠控制",
              done,
            );
            return active;
          },
        );
        if (ended) ctx.ui.notify(ended, "info");
      } finally {
        dispose();
      }
    },
  });
}
