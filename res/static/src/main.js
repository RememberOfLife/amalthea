import { render } from "../vendor/web.js";
import html from "../vendor/html.js";
import {
    createSignal,
    onMount,
    onCleanup,
    For,
    Show,
} from "../vendor/solid.js";
import { api, base, endpoint, json, bytes } from "./api.js";
import { safeName } from "./transfer-utils.js";
import { transferProgress } from "./transfer-progress.js";
import {
    jobs,
    configureTransfers,
    setRate,
    pause,
    pauseAll,
    upload,
    restoreUploads,
    browserStorage,
    refreshBrowserStorage,
    clearBrowserStorage,
    download,
    resumeDownload,
    saveDownload,
    discard,
} from "./transfers.js";

const [error, setError] = createSignal("");
const run = async (action) => {
    setError("");
    try {
        return await action();
    } catch (e) {
        setError(e.message);
    }
};
const date = (seconds) => {
    if (seconds == null) return "Never";
    const value = new Date(seconds * 1000);
    const pad = (part) => String(part).padStart(2, "0");
    const day = [
        value.getFullYear(),
        pad(value.getMonth() + 1),
        pad(value.getDate()),
    ].join("-");
    const time = [value.getHours(), value.getMinutes(), value.getSeconds()]
        .map(pad)
        .join(":");
    return `${day} ${time}`;
};
const capable = () =>
    !!(
        window.isSecureContext &&
        navigator.storage?.getDirectory &&
        navigator.locks
    );
const alert = () =>
    error() &&
    html`<div class="alert alert-danger" role="alert">
        ${error}<button
            class="btn-close float-end"
            aria-label="Dismiss error"
            onClick=${() => setError("")}
        ></button>
    </div>`;

function storageQuota(account) {
    const used = account.used_bytes;
    const reserved = account.reserved_bytes;
    const quota = account.user.quota_bytes;
    const total = used + reserved;
    const free = Math.max(0, quota - total);
    const over = Math.max(0, total - quota);
    const scale = Math.max(quota, total, 1);
    // Make room for the excess without changing the used-to-reserved ratio.
    const withinQuota = total > quota ? quota / total : 1;
    const summary = `${bytes(used)} used · ${bytes(reserved)} reserved · ${bytes(quota)} quota`;
    const segments = [
        {
            label: "Used",
            value: used,
            width: used * withinQuota,
            color: "bg-primary",
        },
        {
            label: "Reserved",
            value: reserved,
            width: reserved * withinQuota,
            color: "bg-info text-dark",
        },
        { label: "Free", value: free, width: free, color: "bg-success" },
        { label: "Over quota", value: over, width: over, color: "bg-danger" },
    ];
    return html`
        <p class="mt-3 mb-1">${summary}</p>
        <div
            class="progress"
            role="img"
            aria-label=${`${summary} · ${bytes(free)} free · ${bytes(over)} over quota`}
        >
            ${segments
                .filter((segment) => segment.width > 0)
                .map(
                    (segment) => html`
                        <div
                            class=${`progress-bar ${segment.color}`}
                            style=${`width: ${(segment.width / scale) * 100}%`}
                            title=${`${bytes(segment.value)} ${segment.label.toLowerCase()}`}
                            aria-hidden="true"
                        >
                            ${segment.label}
                        </div>
                    `,
                )}
        </div>
    `;
}

function App() {
    const [me, setMe] = createSignal();
    const [files, setFiles] = createSignal([]);
    const [notice, setNotice] = createSignal("");
    const [loading, setLoading] = createSignal(true);
    const [pageError, setPageError] = createSignal();
    const [shared, setShared] = createSignal();
    const [scope, setScope] = createSignal("");
    const [selected, setSelected] = createSignal();
    const [newName, setNewName] = createSignal("");
    const [shareFile, setShareFile] = createSignal();
    const [deleteFile, setDeleteFile] = createSignal();
    const [renameFile, setRenameFile] = createSignal();
    const [filter, setFilter] = createSignal("");
    const path = location.pathname.slice(base.length);
    const token = path.startsWith("/s/") ? path.split("/")[2] : null;
    const managedLink = path.startsWith("/links/") ? path.split("/")[2] : null;
    const linkPage = Boolean(token || managedLink);
    const unavailable = () => Boolean(pageError());
    const refreshShared = async () => {
        const id = shared()?.link?.id || managedLink;
        setShared(await api(id ? `/links/${id}` : `/public/${token}`));
    };
    const sharedContent = () =>
        token ? `/public/${token}/content` : `/files/${shared()?.id}/content`;
    const registration = path === "/register";
    const imprint = path === "/imprint" || path === "/imprint/";
    const invitation = location.hash.slice(1);
    // Serialize account reads so an older response cannot overwrite newer quota.
    let accountRefresh = Promise.resolve();
    const refreshAccount = () => {
        const read = async () => {
            const account = await api("/me");
            setMe(account);
            return account;
        };
        accountRefresh = accountRefresh.then(read, read);
        return accountRefresh;
    };
    let fileRefresh = Promise.resolve();
    const refreshFiles = () => {
        const read = async () => {
            const rows = await api("/files");
            setFiles((previous) => {
                const byId = new Map(previous.map((file) => [file.id, file]));
                // For keys by object identity. Keep unchanged cards mounted so polling
                // does not restart spinners or interrupt interaction with their buttons.
                return rows.map((file) => {
                    const existing = byId.get(file.id);
                    return existing &&
                        Object.keys(existing).length ===
                            Object.keys(file).length &&
                        Object.keys(file).every(
                            (key) => existing[key] === file[key],
                        )
                        ? existing
                        : file;
                });
            });
        };
        fileRefresh = fileRefresh.then(read, read);
        return fileRefresh;
    };
    const refresh = async () => {
        const [account] = await Promise.all([refreshAccount(), refreshFiles()]);
        const owner = `user:${account.user.id}`;
        setScope(owner);
        await restoreUploads(owner);
    };
    // Quota refresh failures must not interrupt an otherwise healthy upload.
    configureTransfers(refresh, () => run(refreshAccount));
    onMount(async () => {
        if (imprint) {
            document.title = "Imprint · Amalthea";
            setLoading(false);
            return;
        }
        try {
            if (linkPage) {
                await refreshShared();
                setScope(token ? `share:${token}` : `link:${managedLink}`);
                if (token)
                    history.replaceState(
                        null,
                        "",
                        `${base}/s/${token}/${encodeURIComponent(shared().name)}`,
                    );
            } else if (!registration) {
                await refresh();
                history.replaceState(
                    null,
                    "",
                    `${base}/${me().user.username}/`,
                );
            }
        } catch (e) {
            if (linkPage)
                setPageError({ status: e.status, message: e.message });
            else if (e.status !== 401) setError(e.message);
        } finally {
            setLoading(false);
        }
    });
    let polling = false;
    const filePoll = setInterval(async () => {
        if (!me() || linkPage || polling) return;
        polling = true;
        try {
            await refreshFiles();
        } catch (_) {
            /* Manual refresh reports errors. */
        } finally {
            polling = false;
        }
    }, 2000);
    onCleanup(() => clearInterval(filePoll));
    const offline = () =>
        setNotice(
            "Connection lost. Resume transfers when you are back online.",
        );
    onMount(() => window.addEventListener("offline", offline));
    onCleanup(() => {
        pauseAll();
        window.removeEventListener("offline", offline);
    });
    const doDownload = (file) =>
        run(async () => {
            if (navigator.storage.persist) await navigator.storage.persist();
            await download(
                file,
                linkPage ? sharedContent() : `/files/${file.id}/content`,
                scope(),
            );
        });
    return html`
        <header class="site-header border-bottom">
            <nav
                class="container py-1 py-sm-2 d-flex flex-wrap justify-content-between align-items-center gap-3"
                aria-label="Main"
            >
                <a class="brand" href=${`${base}/`}>
                    <img
                        class="brand-logo d-block w-auto flex-shrink-0 pe-sm-1"
                        src=${`${base}/res/static/images/amalthea.webp`}
                        alt=""
                        width="1103"
                        height="875"
                    />
                    <span>Amalthea</span>
                </a>
                <${Show} when=${me}
                    ><div
                        class="header-account d-flex align-items-center gap-3"
                    >
                        <span class="header-username"
                            >${() => me()?.user.username}</span
                        ><button
                            class="btn btn-sm btn-outline-secondary flex-shrink-0"
                            title="Sign out"
                            aria-label="Sign out"
                            onClick=${() =>
                                run(async () => {
                                    pauseAll();
                                    await api("/logout", { method: "POST" });
                                    location.assign(`${base}/login`);
                                })}
                        >
                            <i
                                class="bi bi-box-arrow-right"
                                aria-hidden="true"
                            ></i>
                        </button></div
                ><//>
            </nav>
        </header>
        <main
            class=${() =>
                `container py-4${!imprint ? " workspace-container" : ""}${unavailable() ? " workspace-unavailable" : ""}`}
        >
            <${Show} when=${imprint}>
                <section
                    class="imprint mx-auto bg-body-secondary p-3"
                    innerHTML=${document.getElementById("imprint-content")
                        .innerHTML}
                ></section>
            <//>
            <${Show} when=${!imprint}>
                <${Show} when=${unavailable}>
                    <section
                        class="workspace-error text-center"
                        role="alert"
                        aria-labelledby="page-error-title"
                    >
                        <p
                            class="display-4 fw-semibold text-body-secondary mb-3"
                        >
                            ${() => pageError()?.status || "Error"}
                        </p>
                        <h1 id="page-error-title" class="h3">
                            ${() =>
                                pageError()?.status === 404
                                    ? "Not found or no longer available"
                                    : pageError()?.status === 401
                                      ? "Sign in required"
                                      : "Unable to open link"}
                        </h1>
                        <p class="text-body-secondary mb-4">
                            ${() =>
                                pageError()?.status === 404
                                    ? "This link may have expired, been revoked, or been deleted."
                                    : pageError()?.message}
                        </p>
                        <a class="btn btn-primary" href=${`${base}/`}
                            >Back to home</a
                        >
                    </section>
                <//>
                <${Show} when=${() => !unavailable()}>${alert}<//>
                ${() =>
                    notice() &&
                    html`<div class="alert alert-info" role="status">
                        ${notice}
                    </div>`}
                <div class="workspace" hidden=${unavailable}>
                    <${Show}
                        when=${() => !loading()}
                        fallback=${html`<p role="status">Loading...</p>`}
                    >
                        ${() =>
                            !capable() &&
                            html`<p class="alert alert-warning">
                                Durable transfers need a current Firefox or
                                Chrome browser over HTTPS (or localhost). Direct
                                downloads remain available.
                            </p>`}
                        <${Show} when=${() => !me() && !linkPage}
                            ><${AuthForm}
                                registration=${registration}
                                invitation=${invitation}
                        /><//>
                        <${Show} when=${shared}>
                            <section class="shared-file card card-body mb-4">
                                <h1 class="h3 filename">
                                    ${() => shared()?.name}
                                </h1>
                                <p>${() => bytes(shared()?.size)}</p>
                                <${Show} when=${() => shared()?.link}>
                                    <p class="small">
                                        ${() => linkStatus(shared().link)}
                                    </p>
                                    <div class="mb-3">
                                        <${LinkActions}
                                            link=${() => shared().link}
                                            refresh=${(_) => refreshShared()}
                                            deleted=${(_) => {
                                                pauseAll();
                                                setShared();
                                                setNotice(
                                                    "Sharing link and access log deleted. The file is still in your files.",
                                                );
                                            }}
                                        />
                                    </div>
                                <//>
                                <${Show}
                                    when=${() =>
                                        shared()?.can_download !== false}
                                >
                                    <div
                                        class="shared-file-actions d-flex flex-wrap gap-2"
                                    >
                                        <button
                                            class="btn btn-primary"
                                            disabled=${!capable()}
                                            onClick=${() =>
                                                doDownload(shared())}
                                        >
                                            <i
                                                class="bi bi-download me-2"
                                                aria-hidden="true"
                                            ></i>
                                            Download with resume
                                        </button>
                                        <a
                                            class="btn btn-outline-secondary"
                                            href=${() =>
                                                endpoint(sharedContent())}
                                            >Direct download</a
                                        >
                                    </div>
                                    <p
                                        class="small text-body-secondary mt-2 mb-0"
                                    >
                                        The resumable download occupies browser
                                        storage until removed, even after its
                                        done.
                                    </p>
                                <//>
                                <${Show} when=${() => shared()?.link}>
                                    <${AccessLog}
                                        rows=${() => shared()?.access_log || []}
                                        refresh=${(_) => refreshShared()}
                                    />
                                <//>
                            </section>
                        <//>
                        <${Show} when=${me}>
                            <section
                                class="file-quota mb-3"
                                aria-label="Storage quota"
                            >
                                <div
                                    class="d-flex justify-content-between align-items-center flex-wrap gap-2"
                                >
                                    <h1 class="h3 mb-0">Your files</h1>
                                    <button
                                        class="btn btn-sm btn-outline-secondary"
                                        onClick=${() => run(refresh)}
                                    >
                                        <i
                                            class="bi bi-arrow-clockwise pe-1"
                                            aria-hidden="true"
                                        ></i>
                                        Refresh
                                    </button>
                                </div>
                                ${() => me() && storageQuota(me())}
                                ${() =>
                                    me() &&
                                    me().used_bytes + me().reserved_bytes >
                                        me().user.quota_bytes &&
                                    html`<p class="text-danger">
                                        You are
                                        ${() =>
                                            bytes(
                                                me().used_bytes +
                                                    me().reserved_bytes -
                                                    me().user.quota_bytes,
                                            )}
                                        over quota. Delete files or cancel
                                        unfinished uploads to free space.
                                    </p>`}
                            </section>
                            <section
                                class="upload-form card card-body mb-3"
                                aria-labelledby="upload-title"
                            >
                                <h2 id="upload-title" class="h5">
                                    Upload a file
                                </h2>
                                <form
                                    onSubmit=${(event) => {
                                        event.preventDefault();
                                        const file = selected();
                                        if (!file) return;
                                        run(() =>
                                            upload(file, newName(), scope()),
                                        );
                                        setSelected();
                                        event.currentTarget.reset();
                                    }}
                                >
                                    <label
                                        class="visually-hidden"
                                        for="upload-file"
                                        >Select a file to upload</label
                                    >
                                    <div class="input-group upload-controls">
                                        <input
                                            id="upload-file"
                                            class="form-control"
                                            type="file"
                                            disabled=${!capable()}
                                            onChange=${(event) => {
                                                const file =
                                                    event.currentTarget
                                                        .files[0];
                                                setSelected(file);
                                                if (file)
                                                    setNewName(
                                                        safeName(
                                                            file.name,
                                                        ).slice(0, 64),
                                                    );
                                            }}
                                        />
                                        <button
                                            class="btn btn-primary"
                                            disabled=${() =>
                                                !selected() || !capable()}
                                        >
                                            <i
                                                class="bi bi-upload pe-2"
                                                aria-hidden="true"
                                            ></i>
                                            Upload
                                        </button>
                                    </div>
                                    <${Show} when=${selected}
                                        ><div class="mt-2">
                                            <label
                                                class="form-label mb-0"
                                                for="upload-name"
                                                >Stored filename</label
                                            ><input
                                                id="upload-name"
                                                class="form-control"
                                                aria-describedby="upload-name-hint"
                                                value=${newName}
                                                maxlength="64"
                                                pattern="[A-Za-z0-9_][A-Za-z0-9_.]*"
                                                required
                                                onInput=${(e) =>
                                                    setNewName(
                                                        e.currentTarget.value,
                                                    )}
                                            />
                                            <div
                                                id="upload-name-hint"
                                                class="form-text small ps-2"
                                            >
                                                Allowed file names up to 64
                                                chars: A-Z a-z 0-9 _ . (no
                                                leading dot)
                                            </div>
                                        </div><//
                                    >
                                </form>
                            </section>
                            <${Transfers} scope=${scope} files=${files} />
                            <div class="file-filter">
                                <label class="visually-hidden" for="filter"
                                    >Filter files</label
                                ><input
                                    id="filter"
                                    class="form-control mb-3"
                                    type="search"
                                    placeholder="Filter files..."
                                    value=${filter}
                                    onInput=${(e) =>
                                        setFilter(e.currentTarget.value)}
                                />
                            </div>
                            <div class="file-list list-group mb-4">
                                <${For}
                                    each=${() =>
                                        files().filter((f) =>
                                            f.name
                                                .toLowerCase()
                                                .includes(
                                                    filter().toLowerCase(),
                                                ),
                                        )}
                                    fallback=${html`<span
                                        class="d-block text-center text-body-secondary fs-4 py-3"
                                        role="img"
                                        aria-label="No matching files"
                                        ><i
                                            class="bi bi-inbox"
                                            aria-hidden="true"
                                        ></i
                                    ></span>`}
                                >
                                    ${(file) =>
                                        html`<article
                                            class="file-row list-group-item p-3"
                                        >
                                            <div class="file-info">
                                                <h2 class="h6 filename mb-1">
                                                    ${file.name}
                                                </h2>
                                                <span
                                                    class="small text-body-secondary"
                                                    >${bytes(file.size)}</span
                                                >
                                                <p
                                                    class="small mb-0"
                                                    hidden=${file.status !==
                                                    "failed"}
                                                >
                                                    ${file.processing_error ||
                                                    "Processing failed"}
                                                </p>
                                                <p
                                                    class="small text-warning-emphasis mb-0"
                                                    hidden=${!file.incomplete}
                                                >
                                                    Incomplete — needs a valid
                                                    filename.
                                                </p>
                                            </div>
                                            <div
                                                class="d-flex align-items-center gap-2"
                                            >
                                                <span
                                                    class="small text-body-secondary d-inline-flex align-items-center text-nowrap"
                                                    role="status"
                                                    hidden=${file.status !==
                                                    "processing"}
                                                    ><span
                                                        class="spinner-border spinner-border-sm me-2"
                                                        aria-hidden="true"
                                                    ></span
                                                    >Processing...</span
                                                >
                                                <div
                                                    class="btn-group btn-group-sm"
                                                    role="group"
                                                    aria-label=${`Actions for ${file.name}`}
                                                >
                                                    <button
                                                        class="btn btn-sm btn-outline-primary"
                                                        hidden=${file.incomplete ||
                                                        file.status ===
                                                            "processing"}
                                                        disabled=${file.status !==
                                                            "ready" ||
                                                        file.incomplete ||
                                                        !capable()}
                                                        onClick=${() =>
                                                            doDownload(file)}
                                                    >
                                                        <i
                                                            class="bi bi-download pe-1"
                                                            aria-hidden="true"
                                                        ></i>
                                                        Download
                                                    </button>
                                                    <a
                                                        class="btn btn-sm btn-outline-secondary"
                                                        hidden=${file.status !==
                                                            "ready" ||
                                                        file.incomplete}
                                                        href=${endpoint(
                                                            `/files/${file.id}/content`,
                                                        )}
                                                        title="Use the browser's normal download manager"
                                                        ><i
                                                            class="bi bi-download pe-1"
                                                            aria-hidden="true"
                                                        ></i>
                                                        Direct</a
                                                    >
                                                    <button
                                                        class="btn btn-sm btn-outline-secondary"
                                                        title="Share"
                                                        aria-label="Share"
                                                        hidden=${file.incomplete ||
                                                        file.status ===
                                                            "processing"}
                                                        disabled=${file.status !==
                                                            "ready" ||
                                                        file.incomplete}
                                                        onClick=${() =>
                                                            setShareFile(file)}
                                                    >
                                                        <i
                                                            class="bi bi-share"
                                                            aria-hidden="true"
                                                        ></i>
                                                    </button>
                                                    <button
                                                        class=${`btn btn-sm btn-outline-secondary${file.incomplete ? " rounded-start ms-0" : ""}`}
                                                        title=${file.incomplete
                                                            ? "Fix name"
                                                            : "Rename"}
                                                        aria-label=${file.incomplete
                                                            ? "Fix name"
                                                            : "Rename"}
                                                        hidden=${file.status ===
                                                        "processing"}
                                                        disabled=${file.status !==
                                                        "ready"}
                                                        onClick=${() =>
                                                            setRenameFile(file)}
                                                    >
                                                        <i
                                                            class="bi bi-pencil"
                                                            aria-hidden="true"
                                                        ></i>
                                                    </button>
                                                    <button
                                                        class="btn btn-sm btn-outline-secondary"
                                                        hidden=${file.status !==
                                                            "failed" ||
                                                        file.incomplete}
                                                        onClick=${() =>
                                                            run(async () => {
                                                                await api(
                                                                    `/files/${file.id}/retry`,
                                                                    {
                                                                        method: "POST",
                                                                    },
                                                                );
                                                                await refresh();
                                                            })}
                                                    >
                                                        Retry processing
                                                    </button>
                                                    <button
                                                        class=${`btn btn-sm btn-outline-danger${file.status === "processing" ? " rounded-start ms-0" : ""}`}
                                                        title="Delete"
                                                        aria-label="Delete"
                                                        onClick=${() =>
                                                            setDeleteFile(file)}
                                                    >
                                                        <i
                                                            class="bi bi-x-lg"
                                                            aria-hidden="true"
                                                        ></i>
                                                    </button>
                                                </div>
                                            </div>
                                        </article>`}
                                <//>
                            </div>
                        <//>
                        <${Show} when=${() => !me()}>
                            <${Transfers} scope=${scope} />
                        <//>
                    <//>
                    <${BrowserStorage} />
                </div>
            <//>
        </main>
        <${Show} when=${deleteFile}
            >${(file) =>
                html`<${Modal}
                    title="Permanently delete file?"
                    close=${(_) => setDeleteFile()}
                >
                    <p>
                        <code class="filename me-1">${() => file().name}</code>
                        and all its sharing links will be removed. There is no
                        trash or undo.
                    </p>
                    <div class="text-end">
                        <button
                            class="btn btn-danger"
                            onClick=${() =>
                                run(async () => {
                                    await api(`/files/${file().id}`, {
                                        method: "DELETE",
                                    });
                                    setDeleteFile();
                                    await refresh();
                                })}
                        >
                            Permanently delete
                        </button>
                    </div>
                <//>`}<//
        >
        <${Show} when=${renameFile}
            >${(file) =>
                html`<${RenameModal}
                    file=${file()}
                    close=${(_) => setRenameFile()}
                    refresh=${(_) => refresh()}
                />`}<//
        >
        <${Show} when=${shareFile}
            >${(file) =>
                html`<${ShareModal}
                    file=${file()}
                    close=${(_) => setShareFile()}
                />`}<//
        >
        <footer class="site-footer">
            <${Show} when=${() => me() || shared()}>
                <details class="container py-0">
                    <summary>advanced</summary>
                    <div class="row g-3 my-1">
                        ${["upload", "download"].map(
                            (kind) =>
                                html`<div class="col-sm-6 mt-0">
                                    <label
                                        class="form-label mb-0"
                                        for=${`${kind}-speed`}
                                        >${kind === "upload"
                                            ? "Upload"
                                            : "Download"}
                                        -Throttle (KiB/s; 0 = unlimited)</label
                                    ><input
                                        id=${`${kind}-speed`}
                                        class="form-control"
                                        type="number"
                                        min="0"
                                        value="0"
                                        onInput=${(e) =>
                                            setRate(
                                                kind,
                                                e.currentTarget.value,
                                            )}
                                    />
                                </div>`,
                        )}
                    </div>
                </details>
            <//>
            <div class="mt-3 py-3 bg-body-secondary">
                <div
                    class="container d-flex justify-content-between align-items-center gap-3"
                >
                    <${Show}
                        when=${document.querySelector(
                            'meta[name="amalthea-imprint"]',
                        )?.content === "true"}
                        fallback=${html`<span class="text-secondary"
                            >PRIVATE</span
                        >`}
                    >
                        <a class="link-secondary" href=${`${base}/imprint`}
                            >imprint</a
                        >
                    <//>
                    <div
                        class="d-flex flex-wrap justify-content-end align-items-center gap-2 text-secondary"
                    >
                        <span
                            >Amalthea
                            v${document.querySelector(
                                'meta[name="amalthea-version"]',
                            ).content}</span
                        >
                        <a
                            class="link-secondary"
                            href="https://github.com/RememberOfLife/amalthea"
                            >open-source</a
                        >
                    </div>
                </div>
            </div>
        </footer>
    `;
}

function AuthForm(props) {
    const [busy, setBusy] = createSignal(false);
    const [idStatus, setIdStatus] = createSignal("idle");
    const idStatuses = {
        idle: ["secondary", "<>", "Enter a UserId"],
        checking: ["secondary", "...", "Checking availability"],
        available: ["success", "OK", "UserId is available"],
        unavailable: ["danger", "N/A", "UserId is invalid or unavailable"],
        error: ["warning", "?", "Could not check availability; try again"],
    };
    let idTimer;
    let idVersion = 0;
    const checkId = (event) => {
        if (!props.registration) return;
        const username = event.currentTarget.value;
        const version = ++idVersion;
        clearTimeout(idTimer);
        if (!username) return setIdStatus("idle");
        if (!/^[A-Za-z0-9_]{1,64}$/.test(username))
            return setIdStatus("unavailable");
        setIdStatus("checking");
        idTimer = setTimeout(async () => {
            try {
                const result = await api(
                    "/register/check-username",
                    json("POST", { username, token: props.invitation }),
                );
                if (version === idVersion)
                    setIdStatus(result.available ? "available" : "unavailable");
            } catch {
                if (version === idVersion) setIdStatus("error");
            }
        }, 300);
    };
    onCleanup(() => {
        clearTimeout(idTimer);
        ++idVersion;
    });
    const validateConfirmation = (form) => {
        const confirmation = form.elements.namedItem("password-confirmation");
        if (!confirmation) return;
        confirmation.setCustomValidity(
            confirmation.value &&
                confirmation.value !== form.elements.namedItem("password").value
                ? "UserSecret entries must match."
                : "",
        );
    };
    return html`<section class="auth mx-auto">
        <h1 class="h3 fw-normal text-center mb-4">
            ${props.registration ? "Registration" : "Authentication"}
        </h1>
        <${Show}
            when=${!props.registration || props.invitation}
            fallback=${html`<p>
                You need a registration link from the server operator.
            </p>`}
        >
            <form
                onInput=${(event) => validateConfirmation(event.currentTarget)}
                onSubmit=${(event) => {
                    event.preventDefault();
                    validateConfirmation(event.currentTarget);
                    if (!event.currentTarget.reportValidity()) return;
                    const values = new FormData(event.currentTarget);
                    setBusy(true);
                    run(async () => {
                        const account = await api(
                            props.registration ? "/register" : "/login",
                            json("POST", {
                                username: values.get("username"),
                                password: values.get("password"),
                                ...(props.registration
                                    ? { token: props.invitation }
                                    : {}),
                            }),
                        );
                        location.assign(`${base}/${account.username}/`);
                    }).finally(() => setBusy(false));
                }}
            >
                <div
                    class="d-flex justify-content-between align-items-center mb-1"
                >
                    <label class="form-label mb-0" for="username"
                        ><span class="text-body-secondary">User</span
                        ><b>Id</b></label
                    >${props.registration &&
                    html`<span
                        id="username-status"
                        class=${() =>
                            `badge rounded-pill text-bg-${idStatuses[idStatus()][0]}`}
                        role="status"
                        title=${() => idStatuses[idStatus()][2]}
                        aria-label=${() => idStatuses[idStatus()][2]}
                        >${() => idStatuses[idStatus()][1]}</span
                    >`}
                </div>
                <input
                    class="form-control"
                    id="username"
                    name="username"
                    onInput=${checkId}
                    aria-describedby=${props.registration
                        ? "username-status"
                        : undefined}
                    autocomplete="username"
                    pattern="[A-Za-z0-9_]+"
                    maxlength="64"
                    required
                />
                <label class="form-label mt-3 mb-1" for="password"
                    ><span class="text-body-secondary">User</span
                    ><b>Secret</b></label
                ><input
                    class="form-control"
                    id="password"
                    name="password"
                    type="password"
                    autocomplete=${props.registration
                        ? "new-password"
                        : "current-password"}
                    minlength=${props.registration ? 12 : 1}
                    maxlength="1024"
                    required
                />
                ${props.registration &&
                html`<p
                        id="password-help"
                        class="small text-body-secondary mt-2 mb-0"
                    >
                        Min. 12 characters.
                    </p>
                    <label
                        class="form-label mt-3 mb-1"
                        for="password-confirmation"
                        ><span class="text-body-secondary">Confirm User</span
                        ><b>Secret</b></label
                    ><input
                        class="form-control"
                        id="password-confirmation"
                        name="password-confirmation"
                        type="password"
                        autocomplete="new-password"
                        minlength="12"
                        maxlength="1024"
                        required
                    />`}
                <button
                    class="btn btn-primary w-100 py-2 mt-3"
                    type="submit"
                    disabled=${busy}
                >
                    ${() =>
                        busy()
                            ? "Please wait..."
                            : props.registration
                              ? "Register"
                              : "Authenticate"}
                </button>
            </form>
        <//>
    </section>`;
}
function Modal(props) {
    let dialog;
    onMount(() => {
        setError("");
        dialog.showModal();
    });
    return html`<dialog
        ref=${(el) => {
            dialog = el;
        }}
        onClose=${props.close}
        onCancel=${props.close}
        aria-label=${props.title}
    >
        <div
            class="d-flex justify-content-between align-items-start gap-3 mb-1"
        >
            <h2 class="h4 filename">${props.title}</h2>
            <button
                class="btn-close"
                aria-label="Close dialog"
                onClick=${props.close}
            ></button>
        </div>
        ${alert}${() => props.children}
    </dialog>`;
}
function RenameModal(props) {
    const [name, setName] = createSignal(props.file.name);
    return html`<${Modal}
        title=${props.file.incomplete ? "Fix filename" : "Rename file"}
        close=${props.close}
        ><form
            onSubmit=${(e) => {
                e.preventDefault();
                run(async () => {
                    await api(
                        `/files/${props.file.id}`,
                        json("PATCH", { name: name() }),
                    );
                    props.close();
                    await props.refresh();
                });
            }}
        >
            <label class="form-label mb-0" for="rename-name">Filename</label
            ><input
                id="rename-name"
                class="form-control"
                value=${name}
                onInput=${(e) => setName(e.currentTarget.value)}
                required
                maxlength="128"
                pattern="[A-Za-z0-9_][A-Za-z0-9_.]*"
            />
            <p class="small text-body-secondary ms-2 mb-2">
                ${props.file.incomplete
                    ? "Choose a name of at most 128 ASCII letters, numbers, underscores and dots, with no leading dot. The file stays unchanged until you save."
                    : "Existing sharing links keep working."}
            </p>
            <div class="text-end">
                <button class="btn btn-primary">Save name</button>
            </div>
        </form><//
    >`;
}
function linkStatus(link) {
    if (link.revoked_at) return `Revoked ${date(link.revoked_at)}`;
    if (link.expires_at && link.expires_at * 1000 <= Date.now())
        return `Expired ${date(link.expires_at)}`;
    return `Expires: ${date(link.expires_at)}`;
}
function LinkActions(props) {
    const [busy, setBusy] = createSignal(false);
    const [copied, setCopied] = createSignal(false);
    const [confirmDelete, setConfirmDelete] = createSignal(false);
    const perform = (action) => {
        setBusy(true);
        return run(action).finally(() => setBusy(false));
    };
    return html`<div>
        <div
            class="btn-group btn-group-sm"
            role="group"
            aria-label="Sharing link actions"
        >
            <button
                class="btn btn-sm btn-outline-primary"
                disabled=${() => busy() || !props.link.url}
                title=${() =>
                    props.link.url
                        ? "Copy sharing link"
                        : "Open the original link once to recover its URL"}
                onClick=${() =>
                    perform(async () => {
                        await navigator.clipboard.writeText(props.link.url);
                        setCopied(true);
                    })}
            >
                ${() => (copied() ? "Copied" : "Copy link")}
            </button>
            <button
                class="btn btn-sm btn-outline-warning"
                disabled=${() => busy() || Boolean(props.link.revoked_at)}
                onClick=${() =>
                    perform(async () => {
                        await api(`/links/${props.link.id}/revoke`, {
                            method: "POST",
                        });
                        await props.refresh();
                    })}
            >
                ${() => (props.link.revoked_at ? "Revoked" : "Revoke")}
            </button>
            <button
                class="btn btn-sm btn-outline-danger"
                disabled=${busy}
                onClick=${() => setConfirmDelete(true)}
            >
                Delete
            </button>
        </div>
        <${Show} when=${() => !props.link.url}>
            <p class="small text-body-secondary mt-2 mb-0">
                Open the original link once to make it copyable.
            </p>
        <//>
        <${Show} when=${confirmDelete}>
            <${Modal}
                title="Delete sharing link?"
                close=${(_) => setConfirmDelete(false)}
            >
                <p>
                    This permanently deletes the link and its access log. The
                    file is kept.
                </p>
                <div class="d-flex justify-content-end gap-2">
                    <button
                        class="btn btn-secondary"
                        disabled=${busy}
                        onClick=${() => setConfirmDelete(false)}
                    >
                        Cancel
                    </button>
                    <button
                        class="btn btn-danger"
                        disabled=${busy}
                        onClick=${() =>
                            perform(async () => {
                                await api(`/links/${props.link.id}`, {
                                    method: "DELETE",
                                });
                                setConfirmDelete(false);
                                await props.deleted();
                            })}
                    >
                        Delete link and log
                    </button>
                </div>
            <//>
        <//>
    </div>`;
}
function AccessLog(props) {
    const [busy, setBusy] = createSignal(false);
    return html`<section class="border-top mt-3 pt-3">
        <div class="d-flex justify-content-between align-items-center gap-2">
            <h2 class="h5 mb-0">Access log</h2>
            <button
                class="btn btn-sm btn-outline-secondary"
                disabled=${busy}
                onClick=${() => {
                    setBusy(true);
                    return run(props.refresh).finally(() => setBusy(false));
                }}
            >
                Refresh log
            </button>
        </div>
        <p class="small text-body-secondary mt-0 mb-2">
            Latest 200 non-owning link visits. Only you can see this log.
        </p>
        <ul class="list-unstyled small log mb-0">
            <${For}
                each=${() => props.rows}
                fallback=${html`<li>No access recorded.</li>`}
            >
                ${(entry) => html`<li>${date(entry.at)} · Link opened</li>`}
            <//>
        </ul>
    </section>`;
}
function ShareModal(props) {
    const [links, setLinks] = createSignal([]);
    const [created, setCreated] = createSignal("");
    const [busy, setBusy] = createSignal(false);
    const refresh = async () =>
        setLinks(await api(`/files/${props.file.id}/links`));
    onMount(() => run(refresh));
    return html`<${Modal}
        title=${`Share ${props.file.name}`}
        close=${props.close}
    >
        <p class="text-secondary">
            Revoke to stop sharing; delete to remove a link and its access log.
        </p>
        <form
            class="row g-2 mb-3"
            onSubmit=${(e) => {
                e.preventDefault();
                const data = new FormData(e.currentTarget);
                setBusy(true);
                run(async () => {
                    const days = Number(data.get("days"));
                    const result = await api(
                        `/files/${props.file.id}/links`,
                        json("POST", {
                            label: data.get("label"),
                            expires_at: days
                                ? Math.floor(Date.now() / 1000) + days * 86400
                                : null,
                        }),
                    );
                    setCreated(result.url);
                    await refresh();
                }).finally(() => setBusy(false));
            }}
        >
            <div class="col-sm-6">
                <label for="link-label" class="form-label mb-0"
                    >Label (optional)</label
                ><input
                    id="link-label"
                    class="form-control"
                    name="label"
                    maxlength="200"
                    placeholder="..."
                />
            </div>
            <div class="col-sm-6">
                <label for="link-expiry" class="form-label mb-0"
                    >Expires after</label
                ><select id="link-expiry" class="form-select" name="days">
                    <option value="7">7 days</option>
                    <option value="1">1 day</option>
                    <option value="30">30 days</option>
                    <option value="0">Never</option>
                </select>
            </div>
            <div>
                <button class="btn btn-primary" disabled=${busy}>
                    Create link
                </button>
            </div>
        </form>
        <${Show} when=${created}>
            <p class="small" role="status">
                Link created. Copy it or open its download page below.
            </p>
        <//>
        <div class="table-responsive">
            <table class="table align-middle">
                <thead>
                    <tr>
                        <th scope="col">Link</th>
                        <th scope="col">Status</th>
                        <th scope="col">Actions</th>
                    </tr>
                </thead>
                <tbody>
                    <${For}
                        each=${links}
                        fallback=${html`<tr>
                            <td colspan="3">
                                No sharing links. This file is private.
                            </td>
                        </tr>`}
                    >
                        ${(link) =>
                            html`<tr>
                                <td>
                                    <a href=${link.url || link.manage_url}
                                        >${link.label || "Unlabelled link"}</a
                                    >
                                </td>
                                <td class="small">${linkStatus(link)}</td>
                                <td>
                                    <${LinkActions}
                                        link=${link}
                                        refresh=${(_) => refresh()}
                                        deleted=${(_) => refresh()}
                                    />
                                </td>
                            </tr>`}
                    <//>
                </tbody>
            </table>
        </div>
    <//>`;
}
function TransferRow(props) {
    // For is keyed by the immutable transfer ID. Progress changes only these bindings,
    // so a pointer-down / pointer-up pair always lands on the same button element.
    const job = () => jobs().find((row) => row.key === props.transferKey) || {};
    const progress = () =>
        transferProgress(
            job(),
            props.files?.find((file) => file.id === job().uploadId),
        );
    const canResume = () =>
        !job().orphan &&
        !job().complete &&
        (job().kind === "download" || job().file);
    const blocked = () =>
        job().pausing ||
        job().removing ||
        (!job().active && job().busyElsewhere);
    const discardLabel = () =>
        job().removing
            ? "Removing..."
            : job().kind === "download"
              ? job().active
                  ? "Cancel download"
                  : "Remove from browser"
              : job().complete
                ? "Dismiss"
                : "Cancel upload";
    const resumeUpload = (file) => {
        const current = job();
        return upload(
            file,
            current.name,
            current.scope,
            current.uploadId
                ? {
                      id: current.uploadId,
                      size: current.size,
                      sha256: current.sha256,
                  }
                : undefined,
        );
    };
    return html`<article class="list-group-item p-3">
        <strong class="filename d-block">${() => job().name}</strong>
        <span class="small" hidden=${() => !job().orphan}>Leftover data</span>
        <div
            hidden=${() => job().orphan}
            class="progress transfer-progress my-1"
            role="progressbar"
            aria-label=${() => `${job().name} ${job().kind} progress`}
            aria-valuemin="0"
            aria-valuemax="100"
            aria-valuenow=${() =>
                progress().indeterminate ? undefined : progress().percent}
            aria-valuetext=${() =>
                `${progress().status}: ${bytes(job().offset || 0)} / ${bytes(job().size)}`}
            style=${() =>
                `grid-template-columns: ${progress().percent}% minmax(0, 1fr)`}
        >
            <div
                class=${() =>
                    `progress-bar bg-${progress().color}${progress().active ? " progress-bar-striped progress-bar-animated" : ""}`}
            ></div>
            <span
                class=${() =>
                    `transfer-progress-label${progress().labelOnFill && !["warning", "info"].includes(progress().color) ? " text-white" : " text-dark"}`}
                style=${() => `grid-column: ${progress().labelOnFill ? 1 : 2}`}
            >
                <span class="text-nowrap">
                    <i
                        class=${() =>
                            `bi bi-${job().kind === "upload" ? "upload" : "download"} me-1`}
                        aria-hidden="true"
                    ></i>
                    <span class="visually-hidden">
                        ${() =>
                            job().kind === "upload" ? "Upload" : "Download"}
                    </span>
                    ${() => bytes(job().offset || 0)}
                </span>
                <span class="text-nowrap">/ ${() => bytes(job().size)}</span>
            </span>
        </div>
        <p class="small mb-1" role="status">
            ${() =>
                job().busyElsewhere && !job().active
                    ? "In use in another tab. Pause it there before resuming or removing it here."
                    : progress().status}
        </p>
        <div
            class="btn-group btn-group-sm w-100 justify-content-end transfer-actions"
            role="group"
            aria-label=${() => `${job().name} transfer actions`}
        >
            <button
                class="btn btn-sm btn-outline-primary text-start"
                hidden=${() => !job().active && !canResume()}
                disabled=${blocked}
                onClick=${() => {
                    const current = job();
                    if (current.active) pause(current.key);
                    else
                        run(() =>
                            current.kind === "download"
                                ? resumeDownload(current)
                                : resumeUpload(current.file),
                        );
                }}
            >
                ${() =>
                    job().pausing
                        ? "Pausing..."
                        : job().active
                          ? "Pause"
                          : job().kind === "download"
                            ? "Resume / verify"
                            : "Resume"}
            </button>
            <label
                class="btn btn-sm btn-outline-primary text-start mb-0"
                hidden=${() =>
                    job().kind !== "upload" ||
                    job().active ||
                    job().complete ||
                    !!job().file}
            >
                Choose original file to resume<input
                    class="visually-hidden"
                    type="file"
                    disabled=${blocked}
                    onChange=${(e) => {
                        const file = e.currentTarget.files[0];
                        e.currentTarget.value = "";
                        if (file) run(() => resumeUpload(file));
                    }}
                />
            </label>
            <button
                class="btn btn-sm btn-primary text-start"
                hidden=${() => !job().complete || job().kind !== "download"}
                disabled=${blocked}
                onClick=${() => run(() => saveDownload(job()))}
            >
                Save a copy
            </button>
            <button
                class="btn btn-sm btn-outline-secondary flex-grow-0 flex-shrink-0"
                disabled=${blocked}
                title=${discardLabel}
                aria-label=${discardLabel}
                onClick=${() => run(() => discard(job()))}
            >
                <i class="bi bi-x-lg" aria-hidden="true"></i>
            </button>
        </div>
    </article>`;
}
function Transfers(props) {
    const uploads = () =>
        jobs().filter(
            (job) => job.kind === "upload" && job.scope === props.scope,
        );
    return html`<${Show} when=${() => uploads().length > 0}>
        <section
            aria-labelledby="transfers-title"
            class="upload-transfers mb-3"
        >
            <h2 class="h5" id="transfers-title">Uploads</h2>
            <div class="list-group">
                <${For} each=${() => uploads().map((job) => job.key)}>
                    ${(key) =>
                        html`<${TransferRow}
                            transferKey=${key}
                            files=${() => props.files}
                        />`}
                <//>
            </div>
        </section>
    <//>`;
}
function BrowserStorage() {
    const [confirmClear, setConfirmClear] = createSignal(false);
    const [clearing, setClearing] = createSignal(false);
    const downloads = () => jobs().filter((job) => job.kind === "download");
    const total = () =>
        downloads().reduce((sum, job) => sum + (job.storedBytes || 0), 0);
    const unknown = () =>
        downloads().some(
            (job) => job.storageUnknown || job.storedBytes == null,
        );
    const active = () =>
        downloads().some(
            (job) => job.active || job.busyElsewhere || job.removing,
        );
    onMount(() => {
        refreshBrowserStorage();
        const refresh = () => {
            if (!document.hidden && !browserStorage().checking && !clearing())
                refreshBrowserStorage();
        };
        const timer = setInterval(refresh, 3000);
        window.addEventListener("focus", refresh);
        onCleanup(() => {
            clearInterval(timer);
            window.removeEventListener("focus", refresh);
        });
    });
    return html`<section
        aria-labelledby="browser-storage-title"
        class="browser-storage mb-4"
    >
        <h2 id="browser-storage-title" class="h5 mb-2">
            Browser Storage (Downloads)
        </h2>
        <details class="small text-body-secondary mb-3">
            <summary>advanced (help)</summary>
            <div class="d-flex flex-wrap gap-2 mt-2 mb-2">
                <button
                    class="btn btn-sm btn-outline-secondary"
                    disabled=${() => browserStorage().checking || clearing()}
                    onClick=${() => refreshBrowserStorage()}
                >
                    Refresh Storage
                </button>
                <button
                    class="btn btn-sm btn-outline-danger"
                    disabled=${() =>
                        active() ||
                        clearing() ||
                        browserStorage().checking ||
                        (!downloads().length && !browserStorage().error)}
                    onClick=${() => setConfirmClear(true)}
                >
                    Clear Browser Storage Cache
                </button>
            </div>
            <p class="mb-0">
                <b>All</b> downloads kept by this site instance in this browser
                profile. (Including other accounts, sharing links, and leftover
                files.)
                <br />
                Saving a copy does not remove the browser copy; let the save
                finish before removing it.
            </p>
        </details>
        <p class="small" role="status">
            ${() => {
                const state = browserStorage();
                if (state.error)
                    return `Could not fully inspect browser storage: ${state.error} Storage may still contain files.`;
                if (!state.checked) return "Checking browser storage...";
                if (!downloads().length)
                    return html`<span
                        class="d-block text-center text-body-secondary fs-4"
                        role="img"
                        aria-label="Browser storage is empty"
                        ><i class="bi bi-inbox" aria-hidden="true"></i
                    ></span>`;
                return `${downloads().length} entries · ${bytes(total())}${unknown() ? " (+ files in use)" : ""} in browser storage`;
            }}
        </p>
        <div class="list-group">
            <${For} each=${() => downloads().map((job) => job.key)}
                >${(key) => html`<${TransferRow} transferKey=${key} />`}<//
            >
        </div>
        <${Show} when=${confirmClear}
            >${(_) =>
                html`<${Modal}
                    title="Clear all browser downloads?"
                    close=${(_event) => {
                        if (!clearing()) setConfirmClear(false);
                    }}
                >
                    <p>
                        This removes all locally stored downloads and leftover
                        transfer files for this Amalthea site, including other
                        accounts and sharing links. Partial downloads will need
                        to start again. Server files and copies you already
                        saved are kept.
                    </p>
                    <button
                        class="btn btn-danger"
                        disabled=${clearing}
                        onClick=${() => {
                            setClearing(true);
                            run(async () => {
                                await clearBrowserStorage();
                                setConfirmClear(false);
                            }).finally(() => setClearing(false));
                        }}
                    >
                        ${() =>
                            clearing()
                                ? "Clearing..."
                                : "Clear all browser downloads"}
                    </button>
                <//>`}<//
        >
    </section>`;
}
render(App, document.getElementById("app"));
