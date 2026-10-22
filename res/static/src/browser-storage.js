// Only this Amalthea mount's OPFS directory is managed here. Scans never create it.
import { base } from "./api.js";
import { hex } from "./transfer-utils.js";

let namePromise;
export function storageDirectoryName() {
    return (namePromise ??= crypto.subtle
        .digest("SHA-256", new TextEncoder().encode(base))
        .then(
            (value) => `amalthea_${hex(new Uint8Array(value)).slice(0, 16)}`,
        ));
}
export async function storageDirectory(create = false) {
    const root = await navigator.storage.getDirectory();
    try {
        return await root.getDirectoryHandle(await storageDirectoryName(), {
            create,
        });
    } catch (error) {
        if (!create && error.name === "NotFoundError") return null;
        throw error;
    }
}
export async function withStorageLock(mode, action) {
    return navigator.locks.request(
        `${await storageDirectoryName()}:storage`,
        { mode, ifAvailable: true },
        async (lock) => {
            if (!lock)
                throw new Error(
                    "Browser storage is busy in this or another tab. Finish inspection or pause downloads, then try again.",
                );
            return action();
        },
    );
}
export function downloadMetadata(record) {
    return {
        name: record.name,
        size: record.size,
        sha256: record.sha256,
        url: record.url,
        scope: record.scope,
    };
}
export async function writeDownloadMetadata(key, record) {
    const root = await storageDirectory(true);
    const handle = await root.getFileHandle(`${key}.json`, { create: true });
    const writer = await handle.createWritable();
    try {
        await writer.write(JSON.stringify(downloadMetadata(record)));
        await writer.close();
    } catch (error) {
        await writer.abort().catch(() => {});
        throw error;
    }
}
function validMetadata(record) {
    if (
        !record ||
        typeof record.name !== "string" ||
        !Number.isSafeInteger(record.size) ||
        record.size < 0 ||
        typeof record.scope !== "string" ||
        typeof record.url !== "string" ||
        !/^[a-f0-9]{64}$/.test(record.sha256)
    )
        return false;
    const url = new URL(record.url, location.origin);
    return (
        url.origin === location.origin &&
        url.pathname.startsWith(`${base}/api/live/`) &&
        /\/(files|public)\/[A-Za-z0-9_-]+\/content$/.test(url.pathname) &&
        !url.search &&
        !url.hash
    );
}
async function entrySize(handle) {
    if (handle.kind === "file") {
        try {
            return { bytes: (await handle.getFile()).size, unknown: false };
        } catch (error) {
            // Firefox may lock files held by a worker. Still show the entry, never call it empty.
            if (error.name === "NotFoundError")
                return { bytes: 0, unknown: false };
            return { bytes: 0, unknown: true };
        }
    }
    let bytes = 0;
    let unknown = false;
    for await (const child of handle.values()) {
        const size = await entrySize(child);
        bytes += size.bytes;
        unknown ||= size.unknown;
    }
    return { bytes, unknown };
}
export async function scanStoredDownloads() {
    if (!navigator.storage?.getDirectory || !navigator.locks)
        throw new Error(
            "Browser storage cannot be inspected here. Use HTTPS and a current browser.",
        );
    const rows = await withStorageLock("shared", async () => {
        const root = await storageDirectory();
        if (!root) return [];
        const { held = [] } = await navigator.locks.query();
        const groups = new Map();
        for await (const [name, handle] of root.entries()) {
            const match =
                handle.kind === "file" &&
                /^([a-f0-9]{64})\.(json|data|progress)$/.exec(name);
            const key = match ? match[1] : `leftover:${name}`;
            if (!groups.has(key))
                groups.set(key, {
                    key,
                    entries: [],
                    storedBytes: 0,
                    storageUnknown: false,
                    dataBytes: 0,
                    handles: new Map(),
                });
            const group = groups.get(key);
            const size = await entrySize(handle);
            group.entries.push(name);
            group.storedBytes += size.bytes;
            group.storageUnknown ||= size.unknown;
            if (match) {
                group.handles.set(match[2], handle);
                if (match[2] === "data") group.dataBytes = size.bytes;
            }
        }
        const rows = [];
        for (const group of groups.values()) {
            let record;
            try {
                const manifest = await group.handles.get("json")?.getFile();
                if (manifest && manifest.size <= 65536) {
                    const candidate = JSON.parse(await manifest.text());
                    if (validMetadata(candidate)) record = candidate;
                }
            } catch {
                /* Unreadable metadata is visible below as removable leftover data. */
            }
            rows.push({
                ...(record || {
                    name: "Leftover browser data",
                    size: group.storedBytes,
                    scope: "",
                }),
                key: group.key,
                kind: "download",
                orphan: !record,
                entries: group.entries,
                storedBytes: group.storedBytes,
                storageUnknown: group.storageUnknown,
                offset: group.dataBytes,
                busyElsewhere: held.some((lock) => lock.name === group.key),
                status: !record
                    ? "Incomplete or unreadable metadata; remove this leftover data to free space."
                    : group.dataBytes === record.size &&
                        group.handles.has("data")
                      ? "Ready to verify and save"
                      : "Paused",
            });
        }
        return rows;
    });
    if (!rows.length) await removeEmptyStorageDirectory();
    return rows;
}
async function removeEmptyStorageDirectory() {
    // Scans do not leave an empty application directory behind after the last removal.
    await navigator.locks.request(
        `${await storageDirectoryName()}:storage`,
        { mode: "exclusive", ifAvailable: true },
        async (lock) => {
            if (!lock) return;
            const directory = await storageDirectory();
            if (!directory) return;
            for await (const _name of directory.keys()) return;
            const root = await navigator.storage.getDirectory();
            try {
                await root.removeEntry(await storageDirectoryName());
            } catch (error) {
                if (error.name !== "NotFoundError") throw error;
            }
        },
    );
}
export async function removeStoredDownload(job) {
    return withStorageLock("shared", () =>
        navigator.locks.request(
            job.key,
            { ifAvailable: true },
            async (lock) => {
                if (!lock)
                    throw new Error(
                        "This download is open in another tab. Pause it there first.",
                    );
                const root = await storageDirectory();
                if (!root) return;
                // Delete all three files even when a stale scan saw only some of them.
                const entries = /^[a-f0-9]{64}$/.test(job.key)
                    ? ["data", "progress", "json"].map(
                          (suffix) => `${job.key}.${suffix}`,
                      )
                    : job.entries;
                for (const name of entries || []) {
                    try {
                        await root.removeEntry(name, { recursive: true });
                    } catch (error) {
                        if (error.name !== "NotFoundError") throw error;
                    }
                }
            },
        ),
    );
}
export async function removeAllStoredDownloads() {
    return withStorageLock("exclusive", async () => {
        const root = await navigator.storage.getDirectory();
        try {
            await root.removeEntry(await storageDirectoryName(), {
                recursive: true,
            });
        } catch (error) {
            if (error.name !== "NotFoundError") throw error;
        }
    });
}
