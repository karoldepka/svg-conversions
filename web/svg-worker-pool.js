const DEFAULT_POOL_SIZE = Math.max(
  1,
  Math.min(4, globalThis.navigator?.hardwareConcurrency ?? 2),
);

// Jobs handed to one worker before its earlier results come back. A worker only gets more work
// once the page's main thread handles a reply, so with a single job it idles whenever the main
// thread is busy (e.g. rendering the icons it just converted).
const MAX_JOBS_PER_WORKER = 4;

export class SvgWorkerPool {
  #workers = [];
  #queue = [];
  #pending = new Map();
  #nextId = 0;
  #closed = false;

  constructor({ size = DEFAULT_POOL_SIZE, workerUrl = new URL("./svg-worker.js", import.meta.url) } = {}) {
    if (!Number.isInteger(size) || size < 1) {
      throw new TypeError("Worker pool size must be a positive integer");
    }
    for (let index = 0; index < size; index += 1) {
      const worker = new Worker(workerUrl, { type: "module" });
      const slot = { worker, jobs: 0 };
      worker.addEventListener("message", ({ data }) => this.#settle(slot, data));
      worker.addEventListener("error", (event) => this.#failWorker(slot, event));
      this.#workers.push(slot);
    }
  }

  process(svg, options = {}) {
    if (typeof svg !== "string") {
      return Promise.reject(new TypeError("svg must be a string"));
    }
    return this.#enqueue("process", { svg, ...options });
  }

  /**
   * Converts several SVGs with the same options in one worker message; resolves to one
   * `{ result }` or `{ error }` per input, in order. Far fewer messages than one process() call
   * per icon, which matters when the page's main thread handles every reply.
   */
  processMany(svgs, options = {}) {
    if (!Array.isArray(svgs) || svgs.some((svg) => typeof svg !== "string")) {
      return Promise.reject(new TypeError("svgs must be an array of strings"));
    }
    return this.#enqueue("processMany", { svgs, ...options });
  }

  /** Recolors a raster image (e.g. a PNG logo) at an absolute `url`; resolves to a PNG data URL. */
  processRaster(url, options = {}) {
    if (typeof url !== "string") {
      return Promise.reject(new TypeError("url must be a string"));
    }
    return this.#enqueue("processRaster", { url, ...options });
  }

  generateColorMap(primaryColor = "#00acc1") {
    return this.#enqueue("colorMap", { primaryColor });
  }

  close() {
    if (this.#closed) return;
    this.#closed = true;
    const error = new Error("SVG worker pool was closed");
    for (const task of this.#queue.splice(0)) task.reject(error);
    for (const task of this.#pending.values()) task.reject(error);
    this.#pending.clear();
    for (const { worker } of this.#workers) worker.terminate();
  }

  #enqueue(operation, payload) {
    if (this.#closed) {
      return Promise.reject(new Error("SVG worker pool is closed"));
    }
    return new Promise((resolve, reject) => {
      this.#queue.push({ id: ++this.#nextId, operation, payload, resolve, reject });
      this.#dispatch();
    });
  }

  #dispatch() {
    while (this.#queue.length > 0) {
      const slot = this.#workers.reduce((least, candidate) => candidate.jobs < least.jobs ? candidate : least);
      if (slot.jobs >= MAX_JOBS_PER_WORKER) return;
      const task = this.#queue.shift();
      slot.jobs += 1;
      this.#pending.set(task.id, { ...task, slot });
      slot.worker.postMessage({ id: task.id, operation: task.operation, payload: task.payload });
    }
  }

  #settle(slot, { id, result, error }) {
    const task = this.#pending.get(id);
    if (!task) return;
    this.#pending.delete(id);
    slot.jobs -= 1;
    if (error) task.reject(new Error(error));
    else task.resolve(result);
    this.#dispatch();
  }

  #failWorker(slot, event) {
    const error = event.error ?? new Error(event.message || "SVG worker failed");
    for (const task of [...this.#pending.values()].filter((item) => item.slot === slot)) {
      this.#pending.delete(task.id);
      task.reject(error);
    }
    slot.jobs = 0;
    this.#dispatch();
  }
}
