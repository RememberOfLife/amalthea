import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import html from "../res/static/vendor/html.js";
import { createRoot, createSignal } from "../res/static/vendor/solid.js";

const source = readFileSync(
    new URL("../res/static/src/main.js", import.meta.url),
    "utf8",
);
const capture = (strings, ...values) => ({ strings, values });
const clicks = (template) =>
    template.values.filter((_, i) => /onClick=$/.test(template.strings[i]));

// Exercise the actual component bindings through Solid's html tag. This catches
// zero-argument callbacks being unwrapped into promises instead of functions.
function bindComponent(markup, componentName, component, context) {
    let template;
    const Probe = (props) => {
        template = component(props);
        return "";
    };
    new Function(
        "html",
        componentName,
        ...Object.keys(context),
        `return html\`${markup}\`;`,
    )(html, Probe, ...Object.values(context));
    return template;
}

function componentMarkup(name, from = 0) {
    const start = source.indexOf(`<\u0024{${name}}`, from);
    assert.notEqual(start, -1);
    return source.slice(start, source.indexOf("/>", start) + 2);
}

for (const view of ["download page", "sharing table"]) {
    test(`${view}: copy, revoke and confirmed delete keep callable callbacks`, async () => {
        const previousDocument = globalThis.document;
        const previousWindow = globalThis.window;
        globalThis.document = {
            addEventListener() {},
            createElement: () => ({ content: { querySelectorAll: () => [] } }),
        };
        globalThis.window = { document: globalThis.document };
        let dispose;
        try {
            const requests = [];
            const clipboard = [];
            let refreshes = 0;
            let pauses = 0;
            const redirects = [];
            const initial = {
                id: "one",
                url: "https://example.test/s/secret/file.txt",
                revoked_at: null,
            };
            let template;
            createRoot((cleanup) => {
                dispose = cleanup;
                const [link, setLink] = createSignal(initial);
                const refresh = async () => {
                    refreshes++;
                    setLink({ ...link(), revoked_at: 123 });
                };
                const Component = new Function(
                    "createSignal",
                    "html",
                    "run",
                    "api",
                    "navigator",
                    "Show",
                    "Modal",
                    `return (${source.slice(source.indexOf("function LinkActions("), source.indexOf("function AccessLog("))});`,
                )(
                    createSignal,
                    capture,
                    async (action) => action(),
                    async (path, options) =>
                        requests.push([path, options.method]),
                    {
                        clipboard: {
                            writeText: async (url) => clipboard.push(url),
                        },
                    },
                    () => {},
                    () => {},
                );
                const context =
                    view === "download page"
                        ? {
                              shared: () => ({ link: link() }),
                              refreshShared: refresh,
                              pauseAll: () => pauses++,
                              base: "/share",
                              location: { replace: (path) => redirects.push(path) },
                          }
                        : { link: initial, refresh };
                template = bindComponent(
                    componentMarkup(
                        "LinkActions",
                        view === "sharing table"
                            ? source.indexOf("function ShareModal(")
                            : 0,
                    ),
                    "LinkActions",
                    Component,
                    context,
                );
            });
            assert.equal(
                refreshes,
                0,
                "rendering must not call mutation callbacks",
            );
            const [copy, revoke, openDelete, cancelDelete, confirmDelete] =
                clicks(template);
            assert.equal(typeof confirmDelete, "function");
            await copy();
            assert.deepEqual(clipboard, [initial.url]);
            assert.deepEqual(requests, []);
            await revoke();
            assert.deepEqual(requests, [["/links/one/revoke", "POST"]]);
            assert.equal(refreshes, 1);
            openDelete();
            cancelDelete();
            assert.equal(requests.length, 1, "canceling must not delete");
            openDelete();
            await confirmDelete();
            assert.deepEqual(requests[1], ["/links/one", "DELETE"]);
            if (view === "download page") {
                assert.equal(pauses, 1);
                assert.deepEqual(redirects, ["/share/"]);
            } else {
                assert.equal(refreshes, 2);
            }
        } finally {
            dispose?.();
            globalThis.document = previousDocument;
            globalThis.window = previousWindow;
        }
    });
}

test("download page: refresh log calls its callback once per click", async () => {
    const previousDocument = globalThis.document;
    const previousWindow = globalThis.window;
    globalThis.document = {
        addEventListener() {},
        createElement: () => ({ content: { querySelectorAll: () => [] } }),
    };
    globalThis.window = { document: globalThis.document };
    let dispose;
    try {
        let refreshes = 0;
        let template;
        createRoot((cleanup) => {
            dispose = cleanup;
            const Component = new Function(
                "createSignal",
                "html",
                "run",
                "For",
                "date",
                `return (${source.slice(source.indexOf("function AccessLog("), source.indexOf("function ShareModal("))});`,
            )(
                createSignal,
                capture,
                async (action) => action(),
                () => {},
                String,
            );
            template = bindComponent(
                componentMarkup("AccessLog"),
                "AccessLog",
                Component,
                {
                    shared: () => ({ access_log: [] }),
                    refreshShared: async () => {
                        refreshes++;
                    },
                },
            );
        });
        assert.equal(refreshes, 0);
        await clicks(template)[0]();
        assert.equal(refreshes, 1);
        await clicks(template)[0]();
        assert.equal(refreshes, 2);
    } finally {
        dispose?.();
        globalThis.document = previousDocument;
        globalThis.window = previousWindow;
    }
});
