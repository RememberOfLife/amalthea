import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import html from "../res/static/vendor/html.js";
import { createRoot, createSignal } from "../res/static/vendor/solid.js";
import { transferProgress } from "../res/static/src/transfer-progress.js";

test("transfer rows accept Solid's unwrapped files prop and track server updates", () => {
    // Component-only templates need an empty template shell, not a browser DOM.
    const previousDocument = globalThis.document;
    const previousWindow = globalThis.window;
    globalThis.document = {
        addEventListener() {},
        createElement() {
            return { content: { querySelectorAll: () => [] } };
        },
    };
    globalThis.window = { document: globalThis.document };
    try {
        const source = readFileSync(
            new URL("../res/static/src/main.js", import.meta.url),
            "utf8",
        );
        const rowSource = source.slice(
            source.indexOf("function TransferRow("),
            source.indexOf("function Transfers("),
        );
        const jobs = () => [
            {
                key: "upload",
                kind: "upload",
                uploadId: "file",
                size: 100,
                offset: 100,
                complete: true,
            },
        ];
        // Capture the real row's template bindings without constructing DOM nodes.
        const Row = new Function(
            "jobs",
            "transferProgress",
            "html",
            `return (${rowSource});`,
        )(jobs, transferProgress, (strings, ...values) => ({
            strings,
            values,
        }));
        createRoot((dispose) => {
            try {
                const [files, setFiles] = createSignal([
                    { id: "file", status: "processing" },
                ]);
                let row;
                const Probe = (props) => {
                    row = Row(props);
                    return "";
                };
                const Parent = (props) =>
                    html`<${Probe}
                        transferKey="upload"
                        files=${() => props.files}
                    />`;
                html`<${Parent} files=${files} />`;
                const barClass =
                    row.values[
                        row.strings.findIndex(
                            (part) =>
                                part.includes("class=") &&
                                part.includes("<div"),
                        )
                    ];
                assert.match(barClass(), /bg-info.*progress-bar-striped/);
                setFiles([{ id: "file", status: "ready" }]);
                assert.equal(barClass(), "progress-bar bg-success");
                setFiles([{ id: "file", status: "failed" }]);
                assert.equal(barClass(), "progress-bar bg-danger");
                // Downloads do not receive a files prop.
                const standalone = Row({ transferKey: "upload" });
                const valueIndex = standalone.strings.findIndex((part) =>
                    part.endsWith("aria-valuenow="),
                );
                assert.equal(standalone.values[valueIndex](), 100);
            } finally {
                dispose();
            }
        });
    } finally {
        globalThis.document = previousDocument;
        globalThis.window = previousWindow;
    }
});
