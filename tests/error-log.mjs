import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { errorLog, reportError } from "../res/static/src/errors.js";

test("errors accumulate across successful actions and exclude deliberate pauses", async () => {
    const source = readFileSync(new URL("../res/static/src/main.js", import.meta.url), "utf8");
    const run = new Function("reportError",
        `${source.slice(source.indexOf("const run ="), source.indexOf("const date ="))}; return run;`,
    )(reportError);
    const before = Date.now() / 1000;
    await run(async () => { throw new Error("Could not rename file"); });
    assert.equal(await run(async () => "success"), "success");
    await run(async () => { throw new Error("Could not delete link"); });
    reportError(new DOMException("Paused", "AbortError"));
    reportError(new Error("Browser storage unavailable"));
    const entries = errorLog();
    assert.deepEqual(entries.map((entry) => entry.message), [
        "Could not rename file", "Could not delete link", "Browser storage unavailable",
    ]);
    assert.ok(entries.every((entry) => entry.at >= before && entry.at <= Date.now() / 1000));
    // Repeated user actions are separate errors, even when the message is identical.
    await run(async () => { throw new Error("Could not rename file"); });
    assert.equal(errorLog().length, 4);
    assert.equal(entries.length, 3);
});
