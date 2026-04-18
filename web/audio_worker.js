import { decodeModuleStream } from "./openmpt_bridge.js";

function formatError(error) {
  if (error instanceof Error) {
    return error.message;
  }
  if (typeof error === "string") {
    return error;
  }
  return `${error}`;
}

let queue = Promise.resolve();

self.onmessage = (event) => {
  const message = event.data || {};
  queue = queue.then(
    () => handleMessage(message),
    () => handleMessage(message),
  );
};

async function handleMessage(message) {
  const { id, bytes, filename, sampleRate } = message;
  if (typeof id !== "number") {
    return;
  }

  try {
    await decodeModuleStream(bytes, filename, sampleRate, (track) => {
      self.postMessage({ id, type: "track", track });
    });
    self.postMessage({ id, type: "done" });
  } catch (error) {
    self.postMessage({ id, type: "error", error: formatError(error) });
  }
}
