import test from "node:test";
import assert from "node:assert/strict";
import { transferProgress } from "../res/static/src/transfer-progress.js";

test("the label moves to the larger segment at halfway", () => {
    for (const [offset, percent, labelOnFill] of [
        [0, 0, false],
        [49, 49, false],
        [50, 50, true],
        [100, 100, true],
        [110, 100, true],
        [-1, 0, false],
    ]) {
        const view = transferProgress({ size: 100, offset });
        assert.equal(view.percent, percent);
        assert.equal(view.labelOnFill, labelOnFill);
    }
    assert.equal(transferProgress({ size: 0 }).percent, 0);
    assert.equal(transferProgress({ size: 0, complete: true }).percent, 100);
});

test("active stages use their phase color and honest progress semantics", () => {
    for (const [phase, color, indeterminate] of [
        ["client", "primary", false],
        ["preparing", "primary", true],
        ["transfer", "success", false],
        ["verifying", "primary", true],
        ["server", "info", true],
    ]) {
        const view = transferProgress({
            phase,
            active: true,
            size: 100,
            offset: 25,
        });
        assert.equal(view.active, true);
        assert.equal(view.color, color);
        assert.equal(view.indeterminate, indeterminate);
        assert.equal(view.percent, indeterminate ? 100 : 25);
    }
});

test("paused, failed, restored, and completed transfers are solid", () => {
    for (const [state, color] of [
        [{ phase: "paused" }, "secondary"],
        [{ phase: "error" }, "danger"],
        [{ phase: "conflict" }, "warning"],
        [{ busyElsewhere: true }, "secondary"],
        [{ active: true, pausing: true, phase: "verifying" }, "secondary"],
        [{ active: true, complete: true, phase: "complete" }, "success"],
    ]) {
        const view = transferProgress({ size: 100, offset: 75, ...state });
        assert.equal(view.active, false);
        assert.equal(view.indeterminate, false);
        assert.equal(view.color, color);
    }
});

test("uploaded files stay striped during server processing and show failures", () => {
    const job = { kind: "upload", complete: true, size: 100, offset: 100 };
    const processing = transferProgress(job, { status: "processing" });
    assert.equal(processing.active, true);
    assert.equal(processing.indeterminate, true);
    assert.equal(processing.color, "info");
    const failed = transferProgress(job, {
        status: "failed",
        processing_error: "Checksum mismatch",
    });
    assert.equal(failed.active, false);
    assert.equal(failed.color, "danger");
    assert.equal(failed.status, "Checksum mismatch");
    const ready = transferProgress(job, { status: "ready" });
    assert.equal(ready.active, false);
    assert.equal(ready.color, "success");
});
