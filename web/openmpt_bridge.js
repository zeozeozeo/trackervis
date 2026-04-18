let initPromise = null;

const INTERACTIVE_SLOT_SET_CHANNEL_MUTE_STATUS = 10;
const INTERACTIVE_SLOT_COUNT = 16;
const BLOCK_FRAMES = 4096;
const SCOPE_SAMPLE_RATE = 4000;
const BLOCKS_PER_YIELD = 8;

function assetUrl(name) {
  const base =
    typeof document !== "undefined" && document.baseURI
      ? document.baseURI
      : typeof self !== "undefined" && self.location
        ? self.location.href
        : globalThis.location?.href ?? globalThis.location?.origin ?? "";
  return new URL(name, base).toString();
}

function yieldToBrowser() {
  return new Promise((resolve) => {
    if (typeof requestAnimationFrame === "function") {
      requestAnimationFrame(() => resolve());
    } else {
      setTimeout(resolve, 0);
    }
  });
}

async function ensureOpenMpt() {
  if (globalThis.libopenmpt && globalThis.libopenmpt._malloc) {
    return globalThis.libopenmpt;
  }
  if (initPromise) {
    return initPromise;
  }

  initPromise = new Promise((resolve, reject) => {
    globalThis.libopenmpt = {
      locateFile(path) {
        if (path.endsWith(".wasm")) {
          return assetUrl("libopenmpt.wasm");
        }
        return assetUrl(path);
      },
      onRuntimeInitialized() {
        resolve(globalThis.libopenmpt);
      },
    };

    if (typeof document !== "undefined" && document.createElement && document.head) {
      const script = document.createElement("script");
      script.src = assetUrl("libopenmpt.js");
      script.async = true;
      script.onerror = () => reject(new Error("failed to load libopenmpt.js"));
      document.head.appendChild(script);
      return;
    }

    (async () => {
      try {
        const response = await fetch(assetUrl("libopenmpt.js"));
        if (!response.ok) {
          throw new Error(`failed to load libopenmpt.js: ${response.status}`);
        }
        const source = await response.text();
        const libopenmpt = globalThis.libopenmpt;
        eval(source);
      } catch (error) {
        reject(error);
      }
    })();
  });

  return initPromise;
}

function allocCString(module, text) {
  const encoded = new TextEncoder().encode(`${text}\0`);
  const ptr = module._malloc(encoded.length);
  module.HEAPU8.set(encoded, ptr);
  return ptr;
}

function readCString(module, ptr) {
  if (!ptr) {
    return "";
  }
  let end = ptr;
  while (module.HEAPU8[end] !== 0) {
    end += 1;
  }
  return new TextDecoder().decode(module.HEAPU8.subarray(ptr, end));
}

function createModuleHandle(module, bytes) {
  const dataPtr = module._malloc(bytes.length);
  module.HEAPU8.set(bytes, dataPtr);

  const errorPtr = module._malloc(4);
  const messagePtr = module._malloc(4);
  module.HEAP32[errorPtr >> 2] = 0;
  module.HEAPU32[messagePtr >> 2] = 0;

  const ext = module._openmpt_module_ext_create_from_memory(
    dataPtr,
    bytes.length,
    0,
    0,
    0,
    0,
    errorPtr,
    messagePtr,
    0,
  );

  const errorMessagePtr = module.HEAPU32[messagePtr >> 2];
  const errorMessage = errorMessagePtr
    ? readCString(module, errorMessagePtr).trim()
    : "";
  if (errorMessagePtr) {
    module._openmpt_free_string(errorMessagePtr);
  }
  module._free(errorPtr);
  module._free(messagePtr);

  if (!ext) {
    module._free(dataPtr);
    throw new Error(errorMessage || "libopenmpt failed to load module");
  }

  const handle = {
    dataPtr,
    extPtr: ext,
    modulePtr: module._openmpt_module_ext_get_module(ext),
  };
  module._openmpt_module_set_repeat_count(handle.modulePtr, 0);
  return handle;
}

function destroyModuleHandle(module, handle) {
  if (!handle) {
    return;
  }
  if (handle.extPtr) {
    module._openmpt_module_ext_destroy(handle.extPtr);
  }
  if (handle.dataPtr) {
    module._free(handle.dataPtr);
  }
}

function getInteractiveSetMute(module, handle) {
  const structSize = INTERACTIVE_SLOT_COUNT * 4;
  const structPtr = module._malloc(structSize);
  module.HEAPU8.fill(0, structPtr, structPtr + structSize);
  const idPtr = allocCString(module, "interactive");
  const ok = module._openmpt_module_ext_get_interface(
    handle.extPtr,
    idPtr,
    structPtr,
    structSize,
  );
  module._free(idPtr);
  if (!ok) {
    module._free(structPtr);
    throw new Error("libopenmpt interactive interface is unavailable");
  }
  const fnIndex =
    module.HEAPU32[(structPtr >> 2) + INTERACTIVE_SLOT_SET_CHANNEL_MUTE_STATUS];
  module._free(structPtr);
  if (!fnIndex) {
    throw new Error("libopenmpt interactive mute interface is unavailable");
  }
  return fnIndex;
}

function setChannelMute(module, handle, channel, mute) {
  const fnIndex = getInteractiveSetMute(module, handle);
  const fn = module.__indirect_function_table.get(fnIndex);
  const ok = fn(handle.extPtr, channel, mute ? 1 : 0);
  if (!ok) {
    throw new Error(`failed to set mute state for channel ${channel}`);
  }
}

function metadata(module, handle, key) {
  const keyPtr = allocCString(module, key);
  const valuePtr = module._openmpt_module_get_metadata(handle.modulePtr, keyPtr);
  module._free(keyPtr);
  return readCString(module, valuePtr).trim();
}

function filenameStem(filename) {
  const slash = filename.lastIndexOf("/");
  const base = slash >= 0 ? filename.slice(slash + 1) : filename;
  const dot = base.lastIndexOf(".");
  return dot > 0 ? base.slice(0, dot) : base;
}

function displayLabel(module, handle, filename) {
  const title = metadata(module, handle, "title");
  const artist = metadata(module, handle, "artist");
  if (artist && title) {
    return `${artist} - ${title}`;
  }
  if (title) {
    return title;
  }
  return filenameStem(filename) || "untitled";
}

function selectSubsong(module, handle, subsongIndex) {
  const ok = module._openmpt_module_select_subsong(handle.modulePtr, subsongIndex);
  if (!ok) {
    throw new Error(`failed to select subsong ${subsongIndex + 1}`);
  }
}

async function renderStereo(module, handle, sampleRate) {
  const scratchPtr = module._malloc(BLOCK_FRAMES * 2 * 4);
  const chunks = [];
  let totalFrames = 0;
  let blocks = 0;

  try {
    for (;;) {
      const frames = module._openmpt_module_read_interleaved_float_stereo(
        handle.modulePtr,
        sampleRate,
        BLOCK_FRAMES,
        scratchPtr,
      );
      if (frames <= 0) {
        break;
      }
      const start = scratchPtr >> 2;
      const end = start + frames * 2;
      const chunk = module.HEAPF32.slice(start, end);
      chunks.push(chunk);
      totalFrames += frames;
      blocks += 1;
      if (blocks % BLOCKS_PER_YIELD === 0) {
        await yieldToBrowser();
      }
    }
  } finally {
    module._free(scratchPtr);
  }

  const left = new Float32Array(totalFrames);
  const right = new Float32Array(totalFrames);
  let offset = 0;
  for (const chunk of chunks) {
    for (let frame = 0; frame < chunk.length / 2; frame += 1) {
      left[offset + frame] = chunk[frame * 2];
      right[offset + frame] = chunk[frame * 2 + 1];
    }
    offset += chunk.length / 2;
  }

  return { left, right, durationSeconds: totalFrames / sampleRate };
}

async function renderScopeMono(module, handle, audioSampleRate, scopeSampleRate) {
  const scratchPtr = module._malloc(BLOCK_FRAMES * 2 * 4);
  const stride = Math.max(1, Math.round(audioSampleRate / scopeSampleRate));
  const chunks = [];
  let accum = 0;
  let count = 0;
  let blocks = 0;

  try {
    for (;;) {
      const frames = module._openmpt_module_read_interleaved_float_stereo(
        handle.modulePtr,
        audioSampleRate,
        BLOCK_FRAMES,
        scratchPtr,
      );
      if (frames <= 0) {
        break;
      }

      const output = [];
      const start = scratchPtr >> 2;
      for (let frame = 0; frame < frames; frame += 1) {
        const left = module.HEAPF32[start + frame * 2];
        const right = module.HEAPF32[start + frame * 2 + 1];
        accum += (left + right) * 0.5;
        count += 1;
        if (count >= stride) {
          output.push(accum / count);
          accum = 0;
          count = 0;
        }
      }
      if (output.length > 0) {
        chunks.push(Float32Array.from(output));
      }

      blocks += 1;
      if (blocks % BLOCKS_PER_YIELD === 0) {
        await yieldToBrowser();
      }
    }
  } finally {
    module._free(scratchPtr);
  }

  if (count > 0) {
    chunks.push(Float32Array.of(accum / count));
  }

  let total = 0;
  for (const chunk of chunks) {
    total += chunk.length;
  }
  const mono = new Float32Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    mono.set(chunk, offset);
    offset += chunk.length;
  }
  return mono;
}

async function decodeModuleTracks(bytes, filename, sampleRate, onTrack) {
  const module = await ensureOpenMpt();
  const fileBytes = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  const probe = createModuleHandle(module, fileBytes);

  try {
    const baseLabel = displayLabel(module, probe, filename);
    const subsongCount = Math.max(
      1,
      module._openmpt_module_get_num_subsongs(probe.modulePtr) | 0,
    );
    const channelCount = Math.max(
      1,
      module._openmpt_module_get_num_channels(probe.modulePtr) | 0,
    );
    const tracks = [];

    for (let subsongIndex = 0; subsongIndex < subsongCount; subsongIndex += 1) {
      const master = createModuleHandle(module, fileBytes);
      try {
        selectSubsong(module, master, subsongIndex);
        const rendered = await renderStereo(module, master, sampleRate);
        const channels = [];

        for (let channel = 0; channel < channelCount; channel += 1) {
          const isolated = createModuleHandle(module, fileBytes);
          try {
            selectSubsong(module, isolated, subsongIndex);
            for (let index = 0; index < channelCount; index += 1) {
              setChannelMute(module, isolated, index, index !== channel);
            }
            channels.push(
              await renderScopeMono(
                module,
                isolated,
                sampleRate,
                SCOPE_SAMPLE_RATE,
              ),
            );
          } finally {
            destroyModuleHandle(module, isolated);
          }
        }

        const label =
          subsongCount > 1
            ? `${baseLabel} (Subsong ${subsongIndex + 1})`
            : baseLabel;
        const track = {
          label,
          filename,
          audioSampleRate: sampleRate,
          scopeSampleRate: SCOPE_SAMPLE_RATE,
          durationSeconds: rendered.durationSeconds,
          audioLeft: rendered.left,
          audioRight: rendered.right,
          channels,
        };
        tracks.push(track);
        if (onTrack) {
          onTrack(track);
        }
      } finally {
        destroyModuleHandle(module, master);
      }
    }

    return tracks;
  } finally {
    destroyModuleHandle(module, probe);
  }
}

export async function decodeModuleStream(bytes, filename, sampleRate, onTrack) {
  await decodeModuleTracks(bytes, filename, sampleRate, onTrack);
}

export async function decodeModule(bytes, filename, sampleRate) {
  return await decodeModuleTracks(bytes, filename, sampleRate, null);
}
