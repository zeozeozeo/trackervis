let worker = null;
let nextRequestId = 1;
const pending = new Map();

function workerUrl() {
  const base =
    typeof document !== "undefined" && document.baseURI
      ? document.baseURI
      : globalThis.location?.href ?? globalThis.location?.origin ?? "";
  return new URL("audio_worker.js", base).toString();
}

function ensureWorker() {
  if (worker) {
    return worker;
  }

  worker = new Worker(workerUrl(), { type: "module" });
  worker.onmessage = (event) => {
    const { id, type, track, tracks, error, ok } = event.data || {};
    const entry = pending.get(id);
    if (!entry) {
      return;
    }
    if (type === "track") {
      try {
        entry.onTrack?.(track);
      } catch (callbackError) {
        pending.delete(id);
        entry.reject(callbackError instanceof Error ? callbackError : new Error(`${callbackError}`));
      }
      return;
    }
    pending.delete(id);
    if (type === "done" || ok === true) {
      entry.resolve(tracks);
    } else {
      entry.reject(new Error(error || "worker decode failed"));
    }
  };
  worker.onerror = (event) => {
    const message = event?.message || "worker error";
    rejectAllPending(message);
    worker = null;
  };
  worker.onmessageerror = () => {
    rejectAllPending("worker message error");
    worker = null;
  };

  return worker;
}

function rejectAllPending(message) {
  for (const [id, entry] of pending) {
    pending.delete(id);
    entry.reject(new Error(message));
  }
}

async function decodeDirectStream(bytes, filename, sampleRate, onTrack) {
  const module = await import(
    new URL(
      "openmpt_bridge.js",
      typeof document !== "undefined" && document.baseURI
        ? document.baseURI
        : globalThis.location?.href ?? globalThis.location?.origin ?? "",
    ).toString(),
  );
  return module.decodeModuleStream(bytes, filename, sampleRate, onTrack);
}

export async function decodeModuleStream(bytes, filename, sampleRate, onTrack = () => {}) {
  let activeWorker;
  try {
    activeWorker = ensureWorker();
  } catch (error) {
    console.warn("audio worker unavailable, falling back to main thread decode", error);
    return decodeDirectStream(bytes, filename, sampleRate, onTrack);
  }

  const requestId = nextRequestId++;
  const fileBytes = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);

  return await new Promise((resolve, reject) => {
    pending.set(requestId, { resolve, reject, onTrack });
    try {
      activeWorker.postMessage(
        { id: requestId, bytes: fileBytes, filename, sampleRate },
        fileBytes.byteLength > 0 ? [fileBytes.buffer] : [],
      );
    } catch (error) {
      pending.delete(requestId);
      reject(error);
    }
  });
}

export async function decodeModule(bytes, filename, sampleRate) {
  const tracks = [];
  await decodeModuleStream(bytes, filename, sampleRate, (track) => {
    tracks.push(track);
  });
  return tracks;
}
