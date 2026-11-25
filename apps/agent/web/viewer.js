// @ts-check

const connect = document.querySelector("#connect");
const audio = document.querySelector("#audio");
const disconnect = document.querySelector("#disconnect");
const status = document.querySelector("#status");
const picture = document.querySelector("#picture");
const metrics = document.querySelector("#metrics");

if (!(connect instanceof HTMLButtonElement)
    || !(audio instanceof HTMLButtonElement)
    || !(disconnect instanceof HTMLButtonElement)
    || !(status instanceof HTMLElement)
    || !(picture instanceof HTMLVideoElement)
    || !(metrics instanceof HTMLOutputElement)) {
  throw new Error("Missing preview elements");
}

/** @type {RTCPeerConnection | undefined} */
let peer;
/** @type {AbortController | undefined} */
let aborter;
let timer = 0;
let invitation = location.hash.slice(1);
history.replaceState(null, "", location.pathname);

const ready = /^[A-Za-z0-9_-]{43}$/.test(invitation);
connect.disabled = !ready;
status.textContent = ready ? "可以观看被控端画面。" : "请使用被控端本机终端给出的新邀请。";

connect.onclick = async () => {
  if (connect.disabled) return;
  connect.disabled = true;
  disconnect.disabled = false;
  status.textContent = "正在建立局域网媒体连接…";
  const current = new RTCPeerConnection({ iceServers: [] });
  const signal = new AbortController();
  peer = current;
  aborter = signal;
  const token = invitation;
  invitation = "";
  const media = new MediaStream();
  picture.srcObject = media;
  current.ontrack = (event) => {
    media.addTrack(event.track);
    if (event.track.kind === "audio") audio.disabled = false;
  };
  current.onconnectionstatechange = () => {
    if (current !== peer) return;
    if (current.connectionState === "connected") {
      status.textContent = "已连接，正在接收画面。";
    } else if (["failed", "disconnected", "closed"].includes(current.connectionState)) {
      end("会话已断开，请使用新邀请重新连接。");
    }
  };
  current.addTransceiver("video", { direction: "recvonly" });
  current.addTransceiver("audio", { direction: "recvonly" });

  try {
    await current.setLocalDescription(await current.createOffer());
    await waitForIce(current, signal.signal);
    const offer = current.localDescription;
    if (!offer) throw new Error("No local description");
    const reply = await fetch("/m1/browser/offer", {
      method: "POST",
      headers: { "Authorization": `Bearer ${token}`, "Content-Type": "application/json" },
      body: JSON.stringify({ sdp: offer.sdp }),
      redirect: "error",
      signal: AbortSignal.any([signal.signal, AbortSignal.timeout(10000)]),
    });
    if (!reply.ok) throw new Error("Host rejected preview");
    const answer = await reply.json();
    if (typeof answer.sdp !== "string") throw new Error("Missing answer");
    await current.setRemoteDescription({ type: "answer", sdp: answer.sdp });
    if (current === peer) timer = window.setInterval(updateMetrics, 500);
  } catch {
    if (peer === current) end("连接失败或邀请已使用，请检查主机后重新获取邀请。");
  }
};

audio.onclick = async () => {
  if (!peer) return;
  picture.muted = !picture.muted;
  audio.textContent = picture.muted ? "播放声音" : "静音";
  await picture.play().catch(() => {});
};

disconnect.onclick = () => end("观看已结束。");
window.addEventListener("pagehide", () => end(""));

/** @param {string} message */
function end(message) {
  const current = peer;
  peer = undefined;
  aborter?.abort();
  aborter = undefined;
  current?.close();
  picture.srcObject = null;
  audio.disabled = true;
  disconnect.disabled = true;
  window.clearInterval(timer);
  status.textContent = message;
}

/** @param {RTCPeerConnection} current @param {AbortSignal} signal */
async function waitForIce(current, signal) {
  const deadline = performance.now() + 5000;
  while (current.iceGatheringState !== "complete") {
    if (signal.aborted || performance.now() >= deadline) throw new Error("ICE deadline");
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}

async function updateMetrics() {
  const current = peer;
  if (!current) return;
  try {
    const report = await current.getStats();
    if (peer !== current) return;
    let frames = 0;
    let audioPackets = 0;
    let codec = "";
    let audioCodec = "";
    let decoder = "";
    report.forEach((entry) => {
      if (entry.type !== "inbound-rtp") return;
      const name = String(report.get(entry.codecId)?.mimeType ?? "");
      if (entry.kind === "video") {
        frames = Number(entry.framesDecoded ?? 0);
        codec = name;
        decoder = String(entry.decoderImplementation ?? "");
      } else if (entry.kind === "audio") {
        audioPackets = Number(entry.packetsReceived ?? 0);
        audioCodec = name;
      }
    });
    metrics.dataset.frames = String(frames);
    metrics.dataset.audioPackets = String(audioPackets);
    metrics.dataset.codec = codec;
    metrics.dataset.audioCodec = audioCodec;
    metrics.dataset.decoder = decoder;
    metrics.textContent = frames > 0
      ? `${picture.videoWidth} × ${picture.videoHeight} · ${codec} · 已解码 ${frames} 帧`
      : "等待首帧…";
  } catch {
    if (current === peer) metrics.textContent = "暂时无法获取媒体统计。";
  }
}
