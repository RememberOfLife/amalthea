import { createSignal } from "../vendor/solid.js";
import { api, endpoint } from "./api.js";
import { reportError } from "./errors.js";
import { CHUNK, hex, throttle } from "./transfer-utils.js";
import {
    storageDirectory,
    storageDirectoryName,
    withStorageLock,
    downloadMetadata,
    writeDownloadMetadata,
    scanStoredDownloads,
    removeStoredDownload,
    removeAllStoredDownloads,
} from "./browser-storage.js";
const createWorker = () =>
    new Worker(new URL("./transfer.worker.js", import.meta.url), {
        type: "module",
    });

export const [jobs, setJobs] = createSignal([]);
const running = new Map();
function track(key, kind, controller) {
    let finish;
    const done = new Promise((resolve) => (finish = resolve));
    const task = { kind, controller, done, finish };
    running.set(key, task);
    return task;
}
let uploadRate = 0;
let downloadRate = 0;
let onUploaded = () => {};
let onQuotaChanged = () => {};
export const configureTransfers = (uploaded, quotaChanged) => {
    onUploaded = uploaded;
    onQuotaChanged = quotaChanged;
};
const patch = (key, value) =>
    setJobs((rows) =>
        rows.map((row) => (row.key === key ? { ...row, ...value } : row)),
    );
const digest = async (value) =>
    hex(new Uint8Array(await crypto.subtle.digest("SHA-256", value)));
export const [browserStorage, setBrowserStorage] = createSignal({
    checked: false,
    checking: false,
    error: "",
    checkedAt: null,
});
let scanQueue = Promise.resolve();
const channel =
    typeof BroadcastChannel === "function"
        ? new BroadcastChannel("amalthea-browser-storage")
        : null;
const notifyStorage = () => channel?.postMessage("changed");
channel?.addEventListener("message", () => {
    refreshBrowserStorage();
});

export function refreshBrowserStorage() {
    const scan = async () => {
        setBrowserStorage((state) => ({ ...state, checking: true }));
        try {
            const stored = await scanStoredDownloads();
            setJobs((current) => {
                const downloads = stored.map((row) => {
                    const previous = current.find((job) => job.key === row.key);
                    if (running.has(row.key) || previous?.removing)
                        return {
                            ...previous,
                            entries: row.entries,
                            storedBytes: row.storedBytes,
                            storageUnknown: row.storageUnknown,
                        };
                    const complete =
                        previous?.complete &&
                        !row.orphan &&
                        !row.storageUnknown &&
                        previous.sha256 === row.sha256 &&
                        row.offset === row.size;
                    return {
                        ...row,
                        complete,
                        phase:
                            !row.busyElsewhere && !row.orphan
                                ? previous?.phase
                                : undefined,
                        status:
                            previous &&
                            !previous.active &&
                            !row.busyElsewhere &&
                            !row.orphan
                                ? previous.status
                                : row.status,
                    };
                });
                // Preserve row order as well as identity while a pointer is over a control.
                const byKey = new Map(downloads.map((job) => [job.key, job]));
                const next = [];
                for (const job of current) {
                    if (job.kind !== "download") next.push(job);
                    else if (byKey.has(job.key)) {
                        next.push(byKey.get(job.key));
                        byKey.delete(job.key);
                    }
                    // A download may be preparing its metadata while a scan is in flight.
                    else if (running.has(job.key)) next.push(job);
                }
                return [...next, ...byKey.values()];
            });
            setBrowserStorage({
                checked: true,
                checking: false,
                error: "",
                checkedAt: Date.now(),
            });
        } catch (error) {
            if (error.message !== browserStorage().error) reportError(error);
            setBrowserStorage((state) => ({
                ...state,
                checking: false,
                error: error.message,
            }));
        }
    };
    scanQueue = scanQueue.then(scan, scan);
    return scanQueue;
}
export async function clearBrowserStorage() {
    if ([...running.values()].some((job) => job.kind === "download"))
        throw new Error("Pause all downloads before clearing browser storage.");
    try {
        await removeAllStoredDownloads();
        setJobs((rows) => rows.filter((job) => job.kind !== "download"));
    } finally {
        notifyStorage();
        await refreshBrowserStorage();
    }
}
export function setRate(kind, value) {
    value = Math.max(0, Number(value) || 0);
    if (kind === "upload") uploadRate = value;
    else downloadRate = value;
    for (const job of running.values())
        if (job.kind === kind)
            job.worker?.postMessage({ type: "rate", rate: value });
}
export function pause(key) {
    const job = running.get(key);
    if (!job || job.controller.signal.aborted) return;
    patch(key, { pausing: true, status: "Pausing..." });
    job.controller.abort();
}
export function pauseAll() {
    for (const key of running.keys()) pause(key);
}
function hashing(file, controller, update) {
    controller.signal.throwIfAborted();
    return new Promise((resolve, reject) => {
        const worker = createWorker();
        const abort = () => {
            worker.terminate();
            reject(new DOMException("Paused", "AbortError"));
        };
        controller.signal.addEventListener("abort", abort, { once: true });
        const finish = () => {
            controller.signal.removeEventListener("abort", abort);
            worker.terminate();
        };
        worker.onerror = () => {
            finish();
            reject(new Error("File hash worker failed"));
        };
        worker.onmessage = ({ data }) => {
            if (data.type === "progress") update(data.offset);
            if (data.type === "hash") {
                finish();
                resolve(data.hash);
            }
            if (data.type === "error") {
                finish();
                reject(new Error(data.message));
            }
        };
        worker.postMessage({ type: "hash", file });
    });
}

export async function upload(file, name, scope, existing) {
    const key = `upload:${scope}:${name}`;
    if (running.has(key)) return;
    if (!jobs().some((j) => j.key === key))
        setJobs((rows) => [
            ...rows,
            { key, name, size: file.size, kind: "upload", scope, offset: 0 },
        ]);
    const previous = jobs().find((j) => j.key === key);
    if (!existing && previous?.uploadId && !previous.complete)
        existing = {
            id: previous.uploadId,
            size: previous.size,
            sha256: previous.sha256,
        };
    if (!existing)
        patch(key, {
            uploadId: undefined,
            sha256: undefined,
            size: file.size,
            offset: 0,
        });
    const controller = new AbortController();
    const task = track(key, "upload", controller);
    patch(key, {
        status: existing ? "Calculating file hash" : "Checking filename",
        phase: "client",
        offset: 0,
        active: true,
        pausing: false,
        complete: false,
        file,
    });
    try {
        await navigator.locks.request(
            key,
            { ifAvailable: true },
            async (lock) => {
                if (!lock)
                    throw new Error("This upload is active in another tab.");
                if (!existing) {
                    await api("/uploads/check-name", {
                        method: "POST",
                        body: JSON.stringify({ name }),
                        signal: controller.signal,
                    });
                    controller.signal.throwIfAborted();
                    patch(key, { status: "Calculating file hash" });
                }
                const hash = await hashing(file, controller, (offset) =>
                    patch(key, { offset }),
                );
                if (
                    existing &&
                    (existing.size !== file.size || existing.sha256 !== hash)
                ) {
                    patch(key, { file: undefined });
                    throw new Error(
                        "Selected file hash does not match the original upload. Choose the original file to resume.",
                    );
                }
                controller.signal.throwIfAborted();
                let state;
                try {
                    state = existing
                        ? await api(`/uploads/${existing.id}`, {
                              signal: controller.signal,
                          })
                        : await api("/uploads", {
                              method: "POST",
                              signal: controller.signal,
                              body: JSON.stringify({
                                  name,
                                  size: file.size,
                                  sha256: hash,
                              }),
                          });
                } catch (error) {
                    if (existing && error.status === 404) {
                        const committed = await api(`/files/${existing.id}`);
                        if (
                            (committed.status !== "ready" ||
                                committed.sha256 === hash) &&
                            committed.size === file.size
                        ) {
                            patch(key, {
                                status: "Uploaded",
                                complete: true,
                                phase: "complete",
                                offset: file.size,
                                file: undefined,
                            });
                            await onUploaded();
                            return;
                        }
                    }
                    throw error;
                }
                patch(key, {
                    uploadId: state.id,
                    size: state.size,
                    sha256: hash,
                    offset: state.offset,
                    status: "Uploading",
                    phase: "transfer",
                });
                // Reservation is for the full file, not just the chunks already sent.
                void onQuotaChanged();
                while (state.offset < state.size) {
                    controller.signal.throwIfAborted();
                    const start = performance.now();
                    const body = await file
                        .slice(state.offset, state.offset + CHUNK)
                        .arrayBuffer();
                    state = await api(`/uploads/${state.id}`, {
                        method: "PUT",
                        body,
                        headers: {
                            "Upload-Offset": String(state.offset),
                            "Upload-Checksum": await digest(body),
                        },
                        signal: controller.signal,
                    });
                    patch(key, { offset: state.offset });
                    await throttle(
                        start,
                        body.byteLength,
                        uploadRate,
                        controller.signal,
                    );
                }
                controller.signal.throwIfAborted();
                patch(key, {
                    status: "Queuing server processing",
                    phase: "server",
                });
                await api(`/uploads/${state.id}/finish`, {
                    method: "POST",
                    signal: controller.signal,
                });
                patch(key, {
                    status: "Uploaded",
                    complete: true,
                    phase: "complete",
                    file: undefined,
                });
                await onUploaded();
            },
        );
    } catch (error) {
        reportError(error);
        patch(key, {
            status: error.name === "AbortError" ? "Paused" : error.message,
            phase:
                error.name === "AbortError"
                    ? "paused"
                    : error.status === 409
                      ? "conflict"
                      : "error",
        });
    } finally {
        running.delete(key);
        patch(key, { active: false, pausing: false });
        task.finish();
        // A request may have committed even when its response was lost or aborted.
        void onQuotaChanged();
    }
}

export async function restoreUploads(scope) {
    const rows = await api("/uploads");
    setJobs((current) => {
        const next = current.filter(
            (j) =>
                j.kind !== "upload" ||
                j.scope !== scope ||
                j.active ||
                j.complete,
        );
        for (const row of rows) {
            const key = `upload:${scope}:${row.name}`;
            if (!next.some((j) => j.key === key))
                next.push({
                    ...row,
                    key,
                    scope,
                    uploadId: row.id,
                    kind: "upload",
                    status: "Paused · choose the original file to resume",
                });
        }
        return next;
    });
}
export async function discard(job) {
    if (job.removing) return;
    patch(job.key, { removing: true });
    try {
        const task = running.get(job.key);
        if (task) {
            pause(job.key);
            await task.done;
            job = jobs().find((row) => row.key === job.key);
            if (!job) return;
        }
        if (job.kind === "upload" && job.uploadId && !job.complete) {
            try {
                await api(`/uploads/${job.uploadId}`, { method: "DELETE" });
            } catch (error) {
                if (error.status !== 404) throw error;
            }
        }
        if (job.kind === "download") await removeStoredDownload(job);
        setJobs((rows) => rows.filter((row) => row.key !== job.key));
        if (job.kind === "upload") await onUploaded();
    } finally {
        patch(job.key, { removing: false });
        if (job.kind === "upload") void onQuotaChanged();
        if (job.kind === "download") {
            notifyStorage();
            await refreshBrowserStorage();
        }
    }
}
export async function download(record, url, scope) {
    if (!navigator.storage?.getDirectory || !navigator.locks)
        throw new Error(
            "Resumable transfers require a current browser and HTTPS (or localhost). Use the direct download link instead.",
        );
    const key = await digest(
        new TextEncoder().encode(`${scope}:${url}:${record.sha256}`),
    );
    if (running.has(key)) return;
    const metadata = {
        name: record.name,
        size: record.size,
        sha256: record.sha256,
        url: endpoint(url),
        scope,
    };
    if (!jobs().some((job) => job.key === key))
        setJobs((rows) => [
            ...rows,
            { ...metadata, key, offset: 0, kind: "download" },
        ]);
    await runDownload({ ...metadata, key }, true);
}
export async function resumeDownload(job) {
    if (job.orphan)
        throw new Error(
            "This leftover data has no usable download metadata; remove it and start a new download.",
        );
    await runDownload(job, false);
}
async function runDownload(job, writeMetadata) {
    if (running.has(job.key)) return;
    const controller = new AbortController();
    const task = track(job.key, "download", controller);
    patch(job.key, {
        status: "Preparing download",
        phase: "preparing",
        active: true,
        pausing: false,
        complete: false,
        busyElsewhere: false,
    });
    try {
        await withStorageLock("shared", () =>
            navigator.locks.request(
                job.key,
                { ifAvailable: true },
                async (lock) => {
                    if (!lock)
                        throw new Error("Download is active in another tab.");
                    controller.signal.throwIfAborted();
                    if (writeMetadata)
                        await writeDownloadMetadata(job.key, job);
                    notifyStorage();
                    const directory = await storageDirectoryName();
                    controller.signal.throwIfAborted();
                    patch(job.key, {
                        status: "Downloading",
                        phase: "transfer",
                    });
                    await new Promise((resolve, reject) => {
                        const worker = createWorker();
                        task.worker = worker;
                        const abort = () =>
                            worker.postMessage({ type: "pause" });
                        const finish = (error) => {
                            controller.signal.removeEventListener(
                                "abort",
                                abort,
                            );
                            worker.terminate();
                            if (error) reject(error);
                            else resolve();
                        };
                        controller.signal.addEventListener("abort", abort, {
                            once: true,
                        });
                        worker.onerror = () =>
                            finish(new Error("Download worker failed"));
                        worker.onmessage = ({ data }) => {
                            if (data.type === "progress")
                                patch(job.key, { offset: data.offset });
                            if (
                                data.type === "verifying" &&
                                !controller.signal.aborted
                            )
                                patch(job.key, {
                                    status: "Verifying file hash",
                                    phase: "verifying",
                                    offset: data.offset,
                                });
                            if (data.type === "complete") {
                                patch(job.key, {
                                    complete: true,
                                    phase: "complete",
                                    offset: data.offset,
                                    status: "Complete · ready to save",
                                });
                                finish();
                            }
                            if (data.type === "error")
                                finish(
                                    data.message === "Paused"
                                        ? new DOMException(
                                              "Paused",
                                              "AbortError",
                                          )
                                        : new Error(data.message),
                                );
                        };
                        // Send plain metadata, not reactive UI state or a retained upload File.
                        worker.postMessage({
                            type: "download",
                            directory,
                            key: job.key,
                            record: downloadMetadata(job),
                            rate: downloadRate,
                        });
                    });
                },
            ),
        );
    } catch (error) {
        reportError(error);
        patch(job.key, {
            status: error.name === "AbortError" ? "Paused" : error.message,
            phase: error.name === "AbortError" ? "paused" : "error",
        });
    } finally {
        running.delete(job.key);
        patch(job.key, { active: false, pausing: false });
        try {
            notifyStorage();
            await refreshBrowserStorage();
        } finally {
            task.finish();
        }
    }
}
export async function saveDownload(job) {
    if (!job.complete) throw new Error("Verify the download before saving it.");
    await withStorageLock("shared", () =>
        navigator.locks.request(
            job.key,
            { ifAvailable: true },
            async (lock) => {
                if (!lock) throw new Error("Download is open in another tab.");
                const root = await storageDirectory();
                if (!root) throw new Error("The local file has been removed.");
                const file = await (
                    await root.getFileHandle(`${job.key}.data`)
                ).getFile();
                const url = URL.createObjectURL(file);
                const a = document.createElement("a");
                a.href = url;
                a.download = job.name;
                a.click();
                setTimeout(() => URL.revokeObjectURL(url), 60_000);
            },
        ),
    );
}
