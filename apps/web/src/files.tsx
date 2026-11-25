import { useCallback, useEffect, useRef, useState } from "react";
import type { ChangeEvent, DragEvent } from "react";
import { browserCore, Capability, type RemoteSession } from "./protocol";

type Stamp = { size: number; sha256: number[] };
type Entry = { name: string; kind: "directory" | "file"; size: number };
type Page = { entries: Entry[]; next: string | null };
type Destination =
  | { state: "writing"; offset: number }
  | { state: "complete"; name: string | null };
type Folder = FileSystemDirectoryHandle & {
  values(): AsyncIterableIterator<
    FileSystemDirectoryHandle | FileSystemFileHandle
  >;
};
type PickerWindow = Window & {
  showDirectoryPicker?: (options: {
    mode: "read" | "readwrite";
  }) => Promise<Folder>;
  showSaveFilePicker?: (options: {
    suggestedName: string;
  }) => Promise<FileSystemFileHandle>;
};

const pickers = window as PickerWindow;
const block = 12 * 1024;
const blobLimit = 64 * 1024 * 1024;
const streamLimit = 512 * 1024 * 1024 * 1024;
const itemLimit = 4096;
let serial = 0;

function permittedName(name: string): string {
  if (
    !name ||
    name === "." ||
    name === ".." ||
    new TextEncoder().encode(name).length > 255 ||
    /[ .]$/u.test(name) ||
    /[<>:"/\\|?*\u0000-\u001f\u007f]/u.test(name)
  )
    throw new Error("文件名不适用于远端文件协议。");
  return name;
}

async function uploadFolder(
  session: RemoteSession,
  initial: Folder,
  destination: string[],
  signal: AbortSignal,
  report: (name: string, done: number, total: number) => void,
) {
  let items = 0;
  async function visit(folder: Folder, parent: string[]) {
    signal.throwIfAborted();
    if (++items > itemLimit || parent.length >= 32)
      throw new Error("目录深度或文件数超过单次传输上限。");
    const name = permittedName(folder.name);
    const { value } = await session.files.request<{ name: string | null }>({
      kind: "directory",
      id: ++serial,
      path: [...parent, name],
      conflict: "keep_both",
    });
    if (!value.name) throw new Error("远端目录未创建。");
    const path = [...parent, permittedName(value.name)];
    for await (const entry of folder.values()) {
      signal.throwIfAborted();
      if (entry.kind === "directory") {
        await visit(entry as Folder, path);
      } else {
        if (++items > itemLimit)
          throw new Error("目录文件数超过单次传输上限。");
        const file = await (entry as FileSystemFileHandle).getFile();
        await pushFile(
          session,
          [...path, permittedName(entry.name)],
          file,
          signal,
          (done) => report(file.name, done, file.size),
        );
      }
    }
  }
  await visit(initial, destination);
}

async function downloadFolder(
  session: RemoteSession,
  source: string[],
  selected: Folder,
  signal: AbortSignal,
  report: (name: string, done: number, total: number) => void,
) {
  const occupied = new Set<string>();
  for await (const child of selected.values()) {
    occupied.add(child.name.toLowerCase());
    if (occupied.size >= 100_000) throw new Error("请选择项目较少的保存目录。");
  }
  const name = distinctName(permittedName(source.at(-1) ?? ""), occupied);
  const root = await selected.getDirectoryHandle(name, { create: true });
  let items = 0;
  async function visit(path: string[], folder: FileSystemDirectoryHandle) {
    if (++items > itemLimit || path.length > 32)
      throw new Error("目录深度或文件数超过单次传输上限。");
    let after: string | null = null;
    const occupied = new Set<string>();
    do {
      signal.throwIfAborted();
      const response: { value: { page: Page } } = await session.files.request({
        kind: "list",
        path,
        after,
      });
      const { entries, next } = response.value.page;
      for (const entry of entries) {
        signal.throwIfAborted();
        const name = permittedName(entry.name);
        const localName = distinctName(name, occupied);
        const nextPath = [...path, name];
        if (entry.kind === "directory") {
          await visit(
            nextPath,
            await folder.getDirectoryHandle(localName, { create: true }),
          );
        } else {
          if (++items > itemLimit)
            throw new Error("目录文件数超过单次传输上限。");
          const file = await folder.getFileHandle(localName, { create: true });
          await pullFile(session, nextPath, file, signal, (done, total) =>
            report(name, done, total),
          );
        }
      }
      if (next && next === after) throw new Error("远端目录页码无效。");
      after = next;
    } while (after !== null);
  }
  await visit(source, root);
}

function distinctName(original: string, occupied: Set<string>) {
  const dot = original.lastIndexOf(".");
  const stem = dot > 0 ? original.slice(0, dot) : original;
  const extension = dot > 0 ? original.slice(dot) : "";
  let name = original;
  for (let copy = 2; occupied.has(name.toLowerCase()); copy++) {
    name = permittedName(`${stem} (${copy})${extension}`);
  }
  occupied.add(name.toLowerCase());
  return name;
}

async function stamp(file: File, signal: AbortSignal): Promise<Stamp> {
  const { FileHash } = await browserCore();
  const hash = new FileHash();
  try {
    for (let offset = 0; offset < file.size; offset += 1024 * 1024) {
      signal.throwIfAborted();
      hash.update(
        new Uint8Array(
          await file.slice(offset, offset + 1024 * 1024).arrayBuffer(),
        ),
      );
    }
    return { size: file.size, sha256: Array.from(hash.digest()) };
  } finally {
    hash.free();
  }
}

async function pushFile(
  session: RemoteSession,
  path: string[],
  file: File,
  signal: AbortSignal,
  report: (bytes: number) => void,
) {
  if (file.size > streamLimit) throw new Error("文件超过传输上限。");
  const id = ++serial;
  const fingerprint = await stamp(file, signal);
  signal.throwIfAborted();
  try {
    const { value } = await session.files.request<{ status: Destination }>({
      kind: "write_open",
      id,
      path,
      stamp: fingerprint,
      conflict: "keep_both",
    });
    if (value.status.state === "complete") return;
    let offset = value.status.offset;
    if (!Number.isSafeInteger(offset) || offset < 0 || offset > file.size)
      throw new Error("远端确认的写入位置无效。");
    while (offset < file.size) {
      signal.throwIfAborted();
      const data = new Uint8Array(
        await file.slice(offset, offset + block).arrayBuffer(),
      );
      const reply = await session.files.request<{ offset: number }>(
        { kind: "write_block", id, offset },
        data,
      );
      offset += data.length;
      if (reply.value.offset !== offset) throw new Error("写入确认不一致。");
      report(offset);
    }
    signal.throwIfAborted();
    await session.files.request({ kind: "commit", id });
  } catch (error) {
    await session.files.request({ kind: "cancel", id }).catch(() => undefined);
    throw error;
  }
}

async function pullFile(
  session: RemoteSession,
  path: string[],
  handle: FileSystemFileHandle | undefined,
  signal: AbortSignal,
  report: (bytes: number, total: number) => void,
) {
  const { value } = await session.files.request<{ stamp: Stamp }>({
    kind: "read_open",
    path,
    expected: null,
  });
  const expected = value.stamp;
  if (
    !Number.isSafeInteger(expected.size) ||
    expected.size < 0 ||
    expected.size > (handle ? streamLimit : blobLimit) ||
    !Array.isArray(expected.sha256) ||
    expected.sha256.length !== 32
  ) {
    throw new Error(
      handle
        ? "远端文件大小或摘要无效。"
        : "当前下载方式限 64 MiB。支持保存位置选择的浏览器可流式下载大文件。",
    );
  }
  signal.throwIfAborted();
  const { FileHash } = await browserCore();
  const writer = await handle?.createWritable();
  const hash = new FileHash();
  const chunks: Uint8Array<ArrayBuffer>[] = [];
  try {
    for (let offset = 0; offset < expected.size; ) {
      signal.throwIfAborted();
      const reply = await session.files.request<{ offset: number }>({
        kind: "read_block",
        offset,
      });
      const data = reply.data.slice();
      if (
        reply.value.offset !== offset ||
        data.length !== Math.min(block, expected.size - offset)
      )
        throw new Error("远端返回的文件块不完整。");
      hash.update(data);
      if (writer) await writer.write(data);
      else chunks.push(data);
      offset += data.length;
      report(offset, expected.size);
    }
    signal.throwIfAborted();
    if (!hash.digest().every((byte, index) => byte === expected.sha256[index]))
      throw new Error("文件校验失败。");
    if (writer) {
      await writer.close();
      return undefined;
    }
    return new Blob(chunks, { type: "application/octet-stream" });
  } catch (error) {
    await writer?.abort().catch(() => undefined);
    throw error;
  } finally {
    hash.free();
  }
}

function announceError(error: unknown) {
  const names: Record<string, string> = {
    denied: "被控端未授权该操作。",
    conflict: "目标已存在同名项目。",
    missing: "远端项目已移动或删除。",
    changed: "传输时源文件发生变化。",
    full: "目标磁盘空间不足。",
    invalid: "路径或文件名无效。",
    limit: "已达到文件数量或大小上限。",
  };
  if (error instanceof DOMException && error.name === "AbortError")
    return "已取消。";
  return error instanceof Error
    ? (names[error.message] ?? error.message)
    : "文件操作失败。";
}

export function Files({ session }: { session: RemoteSession }) {
  const read = Boolean(session.capabilities & Capability.FileRead);
  const write = Boolean(session.capabilities & Capability.FileWrite);
  const [path, setPath] = useState<string[]>([]);
  const [page, setPage] = useState<Page | null>(null);
  const [picked, setPicked] = useState<Folder[]>([]);
  const [local, setLocal] = useState<
    (FileSystemDirectoryHandle | FileSystemFileHandle)[]
  >([]);
  const [pending, setPending] = useState(false);
  const [message, setMessage] = useState("");
  const [download, setDownload] = useState<{ name: string; url: string }>();
  const task = useRef<AbortController | undefined>(undefined);
  const listRevision = useRef(0);

  const list = useCallback(
    async (target: string[], after: string | null = null) => {
      const revision = ++listRevision.current;
      try {
        const { value } = await session.files.request<{ page: Page }>({
          kind: "list",
          path: target,
          after,
        });
        if (revision !== listRevision.current) return;
        setPath(target);
        if (after) {
          setPage((old) => ({
            entries: [...(old?.entries ?? []), ...value.page.entries],
            next: value.page.next,
          }));
        } else setPage(value.page);
      } catch (error) {
        if (revision === listRevision.current) setMessage(announceError(error));
      }
    },
    [session],
  );

  useEffect(() => {
    void list([]);
    return () => {
      listRevision.current++;
      task.current?.abort();
    };
  }, [list]);

  useEffect(() => {
    return () => {
      if (download) URL.revokeObjectURL(download.url);
    };
  }, [download]);

  const browseLocal = async (folders: Folder[]) => {
    const folder = folders.at(-1);
    if (!folder) return;
    const items: (FileSystemFileHandle | FileSystemDirectoryHandle)[] = [];
    for await (const item of folder.values()) {
      items.push(item);
      if (items.length >= 4096) break;
    }
    items.sort((a, b) => a.name.localeCompare(b.name));
    setLocal(items);
    setPicked(folders);
  };

  const start = async (operation: (signal: AbortSignal) => Promise<void>) => {
    if (task.current) return;
    const controller = new AbortController();
    task.current = controller;
    setPending(true);
    setMessage("");
    try {
      await operation(controller.signal);
      setMessage("传输完成，SHA-256 已校验。");
    } catch (error) {
      setMessage(announceError(error));
    } finally {
      task.current = undefined;
      setPending(false);
    }
  };

  const upload = (files: File[]) => {
    if (files.length === 0) return;
    void start(async (signal) => {
      for (const file of files) {
        signal.throwIfAborted();
        const name = permittedName(file.name);
        await pushFile(session, [...path, name], file, signal, (count) =>
          setMessage(`${name} · ${count} / ${file.size} B`),
        );
      }
      await list(path);
    });
  };

  const retrieve = (entry: Entry) => {
    void start(async (signal) => {
      const name = permittedName(entry.name);
      const handle = pickers.showSaveFilePicker
        ? await pickers.showSaveFilePicker({ suggestedName: name })
        : undefined;
      const blob = await pullFile(
        session,
        [...path, name],
        handle,
        signal,
        (bytes, total) => setMessage(`${name} · ${bytes} / ${total} B`),
      );
      if (blob) setDownload({ name, url: URL.createObjectURL(blob) });
    });
  };

  const transferFolder = (folder: Folder) => {
    void start(async (signal) => {
      await uploadFolder(session, folder, path, signal, (name, done, total) =>
        setMessage(`${name} · ${done} / ${total} B`),
      );
      await list(path);
    });
  };

  const retrieveFolder = (entry: Entry) => {
    void start(async (signal) => {
      const folder = await pickers.showDirectoryPicker?.({ mode: "readwrite" });
      if (!folder) throw new Error("浏览器未提供目录保存权限。");
      await downloadFolder(
        session,
        [...path, entry.name],
        folder,
        signal,
        (name, done, total) => setMessage(`${name} · ${done} / ${total} B`),
      );
    });
  };

  function selectFiles(event: ChangeEvent<HTMLInputElement>) {
    upload(Array.from(event.currentTarget.files ?? []));
    event.currentTarget.value = "";
  }

  function drop(event: DragEvent) {
    event.preventDefault();
    if (write && !pending) upload(Array.from(event.dataTransfer.files));
  }

  return (
    <div
      className="files"
      onDragOver={(event) => event.preventDefault()}
      onDrop={drop}
    >
      <section>
        <h3>本机文件</h3>
        <div className="bar">
          {write && (
            <label>
              选文件上传{" "}
              <input
                type="file"
                multiple
                onChange={selectFiles}
                disabled={pending}
              />
            </label>
          )}
          {write && pickers.showDirectoryPicker && (
            <button
              disabled={pending}
              onClick={() => {
                void pickers
                  .showDirectoryPicker?.({ mode: "read" })
                  .then((folder) => browseLocal([folder]))
                  .catch((error: unknown) => setMessage(announceError(error)));
              }}
            >
              选本机目录
            </button>
          )}
        </div>
        {picked.length > 1 && (
          <button
            onClick={() => {
              void browseLocal(picked.slice(0, -1)).catch((error: unknown) =>
                setMessage(announceError(error)),
              );
            }}
          >
            上一级
          </button>
        )}
        {picked.length > 0 && (
          <div className="bar">
            <p>{picked.map(({ name }) => name).join(" / ")}</p>
            <button
              disabled={pending}
              onClick={() => {
                const folder = picked.at(-1);
                if (folder) transferFolder(folder);
              }}
            >
              上传此目录
            </button>
          </div>
        )}
        {local.map((item) => (
          <div className="entry" key={item.name}>
            <span>{item.name}</span>
            <button
              disabled={pending}
              onClick={() => {
                const operation =
                  item.kind === "directory"
                    ? browseLocal([...picked, item as Folder])
                    : (item as FileSystemFileHandle)
                        .getFile()
                        .then((file) => upload([file]));
                void operation.catch((error: unknown) =>
                  setMessage(announceError(error)),
                );
              }}
            >
              {item.kind === "directory" ? "打开" : "上传"}
            </button>
          </div>
        ))}
        {download && (
          <p>
            <a href={download.url} download={download.name}>
              下载 {download.name}
            </a>
          </p>
        )}
      </section>
      <section>
        <h3>远端共享目录</h3>
        <div className="bar">
          <button
            disabled={pending || path.length === 0}
            onClick={() => void list(path.slice(0, -1))}
          >
            上一级
          </button>
          <button disabled={pending} onClick={() => void list(path)}>
            刷新
          </button>
          <span>{path.length === 0 ? "/" : path.join(" / ")}</span>
        </div>
        {page?.entries.map((entry) => (
          <div className="entry" key={entry.name}>
            <span>{entry.name}</span>
            {entry.kind === "directory" ? (
              <div className="bar">
                <button
                  disabled={pending}
                  onClick={() => void list([...path, entry.name])}
                >
                  打开
                </button>
                {read && pickers.showDirectoryPicker && (
                  <button
                    disabled={pending}
                    onClick={() => retrieveFolder(entry)}
                  >
                    下载文件夹
                  </button>
                )}
              </div>
            ) : (
              read && (
                <button disabled={pending} onClick={() => retrieve(entry)}>
                  下载
                </button>
              )
            )}
          </div>
        ))}
        {page?.next && (
          <button disabled={pending} onClick={() => void list(path, page.next)}>
            下一页
          </button>
        )}
      </section>
      <div className="file-status bar">
        <output role="status">{message}</output>
        {pending && (
          <button onClick={() => task.current?.abort()}>取消传输</button>
        )}
      </div>
    </div>
  );
}
