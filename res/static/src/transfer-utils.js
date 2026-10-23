export const CHUNK = 4 * 1024 * 1024;
export const hex = (bytes) =>
    Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
export function safeName(name) {
    return (
        name
            .replace(/[^A-Za-z0-9_.]/g, "_")
            .replace(/^\.+/, "_")
            .slice(0, 128) || "file"
    );
}
export function checkRange(response, start, end, size, etag) {
    if (
        response.status !== 206 ||
        response.headers.get("Content-Range") !==
            `bytes ${start}-${end}/${size}` ||
        response.headers.get("ETag") !== etag
    ) {
        throw new Error(
            "The server returned different file content or an invalid byte range. Discard this download and start again.",
        );
    }
}
export async function throttle(start, count, kibPerSecond, signal) {
    if (!kibPerSecond) return;
    const wait =
        (count / (kibPerSecond * 1024)) * 1000 - (performance.now() - start);
    if (wait <= 0) return;
    await new Promise((resolve, reject) => {
        const abort = () => {
            clearTimeout(timer);
            reject(new DOMException("Paused", "AbortError"));
        };
        const timer = setTimeout(() => {
            signal?.removeEventListener("abort", abort);
            resolve();
        }, wait);
        if (signal?.aborted) abort();
        else signal?.addEventListener("abort", abort, { once: true });
    });
}
