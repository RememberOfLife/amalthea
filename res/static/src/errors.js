import { createSignal } from "../vendor/solid.js";

const [entries, setEntries] = createSignal([]);
export const errorLog = entries;

// Keep errors for this page's lifetime without depending on browser storage.
export function reportError(error) {
    if (error?.name === "AbortError") return;
    setEntries((previous) => [
        ...previous,
        { at: Date.now() / 1000, message: error?.message || String(error) },
    ]);
}
