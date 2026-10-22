import { render } from '../vendor/web.js';
import html from '../vendor/html.js';
import { createSignal, onMount, onCleanup, For, Show } from '../vendor/solid.js';
import { api, base, endpoint, json, bytes } from './api.js';
import { safeName } from './transfer-utils.js';
import { jobs, configureTransfers, setRate, pause, pauseAll, upload, restoreUploads, browserStorage, refreshBrowserStorage, clearBrowserStorage, download, resumeDownload, saveDownload, discard } from './transfers.js';

const [error, setError] = createSignal('');
const run = async action => { setError(''); try { return await action(); } catch (e) { setError(e.message); } };
const date = seconds => seconds ? new Date(seconds * 1000).toLocaleString() : 'Never';
const capable = () => !!(window.isSecureContext && navigator.storage?.getDirectory && navigator.locks);
const alert = () => error() && html`<div class="alert alert-danger" role="alert">${error}<button class="btn-close float-end" aria-label="Dismiss error" onClick=${() => setError('')}></button></div>`;

function App() {
  const [me, setMe] = createSignal();
  const [files, setFiles] = createSignal([]);
  const [notice, setNotice] = createSignal('');
  const [loading, setLoading] = createSignal(true);
  const [shared, setShared] = createSignal();
  const [scope, setScope] = createSignal('');
  const [selected, setSelected] = createSignal();
  const [newName, setNewName] = createSignal('');
  const [shareFile, setShareFile] = createSignal();
  const [deleteFile, setDeleteFile] = createSignal();
  const [renameFile, setRenameFile] = createSignal();
  const [filter, setFilter] = createSignal('');
  const path = location.pathname.slice(base.length);
  const token = path.startsWith('/s/') ? path.split('/')[2] : null;
  const registration = path === '/register';
  const imprint = path === '/imprint' || path === '/imprint/';
  const invitation = location.hash.slice(1);
  // Serialize account reads so an older response cannot overwrite newer quota.
  let accountRefresh = Promise.resolve();
  const refreshAccount = () => {
    const read = async () => { const account = await api('/me'); setMe(account); return account; };
    accountRefresh = accountRefresh.then(read, read);
    return accountRefresh;
  };
  const refresh = async () => {
    const [account, rows] = await Promise.all([refreshAccount(), api('/files')]);
    setFiles(rows);
    const owner = `user:${account.user.id}`;
    setScope(owner);
    await restoreUploads(owner);
  };
  // Quota refresh failures must not interrupt an otherwise healthy upload.
  configureTransfers(refresh, () => run(refreshAccount));
  onMount(async () => {
    if (imprint) { document.title = 'Imprint · Amalthea'; setLoading(false); return; }
    try {
      if (token) {
        const file = await api(`/public/${token}`);
        setShared(file); setScope(`share:${token}`);
        history.replaceState(null, '', `${base}/s/${token}/${file.name}`);
      } else if (!registration) {
        await refresh();
        history.replaceState(null, '', `${base}/${me().user.username}/`);
      }
    } catch (e) { if (e.status !== 401 || token) setError(e.message); }
    finally { setLoading(false); }
  });
  const offline = () => setNotice('Connection lost. Resume transfers when you are back online.');
  onMount(() => window.addEventListener('offline', offline));
  onCleanup(() => { pauseAll(); window.removeEventListener('offline', offline); });
  const doDownload = file => run(async () => {
    if (navigator.storage.persist) await navigator.storage.persist();
    await download(file, token ? `/public/${token}/content` : `/files/${file.id}/content`, scope());
  });
  return html`
    <header class="site-header border-bottom"><nav class="container py-2 d-flex flex-wrap justify-content-between align-items-center gap-3" aria-label="Main">
      <a class="brand" href=${`${base}/`}>
        <img class="brand-logo" src=${`${base}/res/static/images/amalthea.webp`} alt="" width="144" height="72">
        <span>Amalthea</span>
      </a>
      <${Show} when=${me}><div class="header-account d-flex align-items-center gap-3"><span class="header-username">${() => me()?.user.username}</span><button class="btn btn-sm btn-outline-secondary flex-shrink-0" onClick=${() => run(async () => { pauseAll(); await api('/logout', { method: 'POST' }); location.assign(`${base}/login`); })}>Sign out</button></div><//>
    </nav></header>
    <main class="container py-4">
      <${Show} when=${imprint}>
        <section class="imprint mx-auto bg-body-secondary p-3" innerHTML=${document.getElementById('imprint-content').innerHTML}></section>
      <//>
      <${Show} when=${!imprint}>
      ${alert}
      ${() => notice() && html`<div class="alert alert-info" role="status">${notice}</div>`}
      <${Show} when=${() => !loading()} fallback=${html`<p role="status">Loading…</p>`}>
        ${() => !capable() && html`<p class="alert alert-warning">Durable transfers need a current Firefox or Chrome browser over HTTPS (or localhost). Direct downloads remain available.</p>`}
        <${Show} when=${() => !me() && !token}><${AuthForm} registration=${registration} invitation=${invitation}/><//>
        <${Show} when=${shared}>
          <section class="card card-body mb-4"><h1 class="h3 filename">${() => shared()?.name}</h1><p>${() => bytes(shared()?.size)}</p>
            <div class="d-flex flex-wrap gap-2"><button class="btn btn-primary" disabled=${!capable()} onClick=${() => doDownload(shared())}><i class="bi bi-download" aria-hidden="true"></i> Download with resume</button>
              <a class="btn btn-outline-secondary" href=${endpoint(`/public/${token}/content`)}>Direct download</a></div>
            <p class="small text-body-secondary mt-3 mb-0">The resumable download stays in this browser until you remove it from browser storage, including after saving a copy. Anyone with this link can download until it expires or is revoked.</p>
          </section>
        <//>
        <${Show} when=${me}>
          <section class="mb-4" aria-label="Storage quota">
            <div class="d-flex justify-content-between align-items-center flex-wrap gap-2"><h1 class="h3 mb-0">Your files</h1><button class="btn btn-sm btn-outline-secondary" onClick=${() => run(refresh)}><i class="bi bi-arrow-clockwise" aria-hidden="true"></i> Refresh</button></div>
            <p class="mt-3 mb-1">${() => me() && `${bytes(me().used_bytes)} used · ${bytes(me().reserved_bytes)} reserved · ${bytes(me().user.quota_bytes)} quota`}</p>
            <progress class="quota" max=${() => Math.max(1, me()?.user.quota_bytes || 0)} value=${() => (me()?.used_bytes || 0) + (me()?.reserved_bytes || 0)} aria-label="Storage used and reserved"></progress>
            ${() => me() && me().used_bytes + me().reserved_bytes > me().user.quota_bytes && html`<p class="text-danger">You are over quota. Delete files or cancel unfinished uploads to free space.</p>`}
          </section>
          <section class="card card-body mb-4" aria-labelledby="upload-title">
            <h2 id="upload-title" class="h5">Upload a file</h2>
            <form onSubmit=${event => { event.preventDefault(); const file = selected(); if (!file) return; run(() => upload(file, newName(), scope())); setSelected(); event.currentTarget.reset(); }}>
              <div class="row g-3 align-items-end"><div class="col-md-6">
                <input id="upload-file" class="form-control" type="file" disabled=${!capable()} onChange=${event => { const file = event.currentTarget.files[0]; setSelected(file); if (file) setNewName(safeName(file.name)); }}/></div>
                <${Show} when=${selected}><div class="col-md-4"><label class="form-label" for="upload-name">Stored filename</label><input id="upload-name" class="form-control" value=${newName} maxLength="200" pattern="[A-Za-z0-9_][A-Za-z0-9_.]*" required onInput=${e => setNewName(e.currentTarget.value)}/></div><//>
                <div class="col-md-auto"><button class="btn btn-primary" disabled=${() => !selected() || !capable()}><i class="bi bi-upload" aria-hidden="true"></i> Upload</button></div>
              </div>
              <p class="small text-body-secondary mb-0 mt-2">Private until you create a sharing link. Filenames use letters, numbers, underscores and dots. Full-file verification runs before uploading.</p>
            </form>
          </section>
          <label class="visually-hidden" for="filter">Filter files</label><input id="filter" class="form-control mb-3" type="search" placeholder="Filter files…" value=${filter} onInput=${e => setFilter(e.currentTarget.value)}/>
          <div class="file-list mb-4"><${For} each=${() => files().filter(f => f.name.toLowerCase().includes(filter().toLowerCase()))} fallback=${html`<p class="text-body-secondary py-3">No matching files.</p>`}>
            ${file => html`<article class="file-row border-bottom py-3">
              <div class="file-info"><h2 class="h6 filename mb-1">${file.name}</h2><span class="small text-body-secondary">${bytes(file.size)}</span><p class="small text-warning-emphasis mb-0" hidden=${!file.incomplete}>Incomplete — needs a valid filename.</p></div>
              <div class="d-flex gap-2 flex-wrap"><button class="btn btn-sm btn-outline-primary" disabled=${file.incomplete || !capable()} onClick=${() => doDownload(file)}>Download</button>
                <a class="btn btn-sm btn-outline-secondary" hidden=${file.incomplete} href=${endpoint(`/files/${file.id}/content`)} title="Use the browser's normal download manager">Direct</a>
                <button class="btn btn-sm btn-outline-secondary" disabled=${file.incomplete} onClick=${() => setShareFile(file)}>Share</button>
                <button class="btn btn-sm btn-outline-secondary" onClick=${() => setRenameFile(file)}>${file.incomplete ? 'Fix name' : 'Rename'}</button>
                <button class="btn btn-sm btn-outline-danger" onClick=${() => setDeleteFile(file)}>Delete</button>
              </div></article>`}
          <//></div>
        <//>
        <${Transfers} scope=${scope}/>
      <//>
      <${BrowserStorage}/>
      <//>
    </main>
    <${Show} when=${deleteFile}>${file => html`<${Modal} title="Permanently delete file?" close=${_ => setDeleteFile()}>
      <p><strong class="filename">${() => file().name}</strong> and all its sharing links will be removed. There is no trash or undo.</p>
      <button class="btn btn-danger" onClick=${() => run(async () => { await api(`/files/${file().id}`, { method: 'DELETE' }); setDeleteFile(); await refresh(); })}>Permanently delete</button>
    <//>`}<//>
    <${Show} when=${renameFile}>${file => html`<${RenameModal} file=${file()} close=${_ => setRenameFile()} refresh=${_ => refresh()}/>`}<//>
    <${Show} when=${shareFile}>${file => html`<${ShareModal} file=${file()} close=${_ => setShareFile()}/>`}<//>
    <footer class="site-footer">
      <div class="mt-4 py-3 bg-body-secondary">
        <div class="container d-flex justify-content-between align-items-center gap-3">
          <${Show} when=${document.querySelector('meta[name="amalthea-imprint"]')?.content === 'true'} fallback=${html`<span class="text-secondary">PRIVATE</span>`}>
            <a class="link-secondary" href=${`${base}/imprint`}>imprint</a>
          <//>
          <a class="link-secondary" href="https://github.com/RememberOfLife/amalthea">open-source</a>
        </div>
      </div>
    </footer>
  `;
}

function AuthForm(props) {
  const [busy, setBusy] = createSignal(false);
  return html`<section class="auth mx-auto"><h1 class="h3">${props.registration ? 'Create your account' : 'Sign in'}</h1>
    <${Show} when=${!props.registration || props.invitation} fallback=${html`<p>You need a registration link from the server operator.</p>`}>
      <form onSubmit=${event => { event.preventDefault(); const values = new FormData(event.currentTarget); setBusy(true); run(async () => {
        const account = await api(props.registration ? '/register' : '/login', json('POST', { username: values.get('username'), password: values.get('password'), ...(props.registration ? { token: props.invitation } : {}) }));
        location.assign(`${base}/${account.username}/`);
      }).finally(() => setBusy(false)); }}>
        <label class="form-label mt-3" for="username">Username</label><input class="form-control" id="username" name="username" autocomplete="username" pattern="[A-Za-z0-9_]+" maxLength="64" required/>
        <label class="form-label mt-3" for="password">Password</label><input class="form-control" id="password" name="password" type="password" autocomplete=${props.registration ? 'new-password' : 'current-password'} minLength=${props.registration ? 12 : 1} maxLength="1024" required/>
        ${props.registration && html`<p class="small text-body-secondary mt-2">Use at least 12 characters. No email address is required.</p>`}
        <button class="btn btn-primary mt-3" disabled=${busy}>${() => busy() ? 'Please wait…' : props.registration ? 'Create account' : 'Sign in'}</button>
      </form>
    <//>
  </section>`;
}
function Modal(props) {
  let dialog;
  onMount(() => { setError(''); dialog.showModal(); });
  return html`<dialog ref=${el => { dialog = el; }} onClose=${props.close} onCancel=${props.close} aria-label=${props.title}>
    <div class="d-flex justify-content-between align-items-start gap-3 mb-3"><h2 class="h4 filename">${props.title}</h2><button class="btn-close" aria-label="Close dialog" onClick=${props.close}></button></div>
    ${alert}${() => props.children}
  </dialog>`;
}
function RenameModal(props) {
  const [name, setName] = createSignal(props.file.name);
  return html`<${Modal} title=${props.file.incomplete ? 'Fix filename' : 'Rename file'} close=${props.close}><form onSubmit=${e => { e.preventDefault(); run(async () => { await api(`/files/${props.file.id}`, json('PATCH', { name: name() })); props.close(); await props.refresh(); }); }}>
    <label class="form-label" for="rename-name">Filename</label><input id="rename-name" class="form-control mb-3" value=${name} onInput=${e => setName(e.currentTarget.value)} required maxLength="200" pattern="[A-Za-z0-9_][A-Za-z0-9_.]*"/>
    <p class="small text-body-secondary">${props.file.incomplete ? 'Choose a name of at most 200 ASCII letters, numbers, underscores and dots, with no leading dot. The file stays unchanged until you save.' : 'Existing sharing links keep working.'}</p><button class="btn btn-primary">Save name</button>
  </form><//>`;
}
function ShareModal(props) {
  const [links, setLinks] = createSignal([]);
  const [created, setCreated] = createSignal('');
  const [log, setLog] = createSignal();
  const [busy, setBusy] = createSignal(false);
  const [copied, setCopied] = createSignal(false);
  const refresh = async () => setLinks(await api(`/files/${props.file.id}/links`));
  onMount(() => run(refresh));
  return html`<${Modal} title=${`Share ${props.file.name}`} close=${props.close}>
    <p>Each link can be revoked independently. The full secret link is shown only when created; copy it before closing.</p>
    <form class="row g-2 mb-3" onSubmit=${e => { e.preventDefault(); const data = new FormData(e.currentTarget); setBusy(true); run(async () => {
      const days = Number(data.get('days'));
      const result = await api(`/files/${props.file.id}/links`, json('POST', { label: data.get('label'), expires_at: days ? Math.floor(Date.now() / 1000) + days * 86400 : null }));
      setCreated(result.url); setCopied(false); await refresh();
    }).finally(() => setBusy(false)); }}>
      <div class="col-sm-6"><label for="link-label" class="form-label">Label (optional)</label><input id="link-label" class="form-control" name="label" maxLength="200" placeholder="For Alex"/></div>
      <div class="col-sm-6"><label for="link-expiry" class="form-label">Expires after</label><select id="link-expiry" class="form-select" name="days"><option value="7">7 days</option><option value="1">1 day</option><option value="30">30 days</option><option value="0">Never</option></select></div>
      <div><button class="btn btn-primary" disabled=${busy}>Create link</button></div>
    </form>
    <${Show} when=${created}><div class="mb-3"><label class="form-label" for="created-link">New sharing link</label><div class="input-group"><input id="created-link" class="form-control" readonly value=${created} onFocus=${e => e.currentTarget.select()}/><button class="btn btn-outline-primary" onClick=${() => run(async () => { await navigator.clipboard.writeText(created()); setCopied(true); })}>${() => copied() ? 'Copied' : 'Copy'}</button></div></div><//>
    <${For} each=${links} fallback=${html`<p class="text-body-secondary">No sharing links. This file is private.</p>`}>${link => html`<article class="border-top py-3">
      <strong>${link.label || 'Unlabelled link'}</strong><p class="small mb-2">${link.revoked_at ? `Revoked ${date(link.revoked_at)}` : link.expires_at && link.expires_at * 1000 <= Date.now() ? `Expired ${date(link.expires_at)}` : `Expires: ${date(link.expires_at)}`}</p>
      <div class="d-flex gap-2"><button class="btn btn-sm btn-outline-secondary" onClick=${() => run(async () => setLog({ label: link.label, rows: await api(`/links/${link.id}/logs`) }))}>Access log</button>
      ${!link.revoked_at && html`<button class="btn btn-sm btn-outline-danger" onClick=${() => run(async () => { await api(`/links/${link.id}`, { method: 'DELETE' }); await refresh(); })}>Revoke</button>`}</div>
    </article>`}<//>
    <${Show} when=${log}><section class="border-top pt-3"><h3 class="h6">Access log · ${() => log()?.label || 'Unlabelled link'}</h3><p class="small">Latest 200 requests. Download entries mean bytes were requested, not proof of a saved file. Resuming adds range requests.</p>
      <ul class="list-unstyled small log"><${For} each=${() => log()?.rows} fallback=${html`<li>No access recorded.</li>`}>${entry => html`<li>${date(entry.at)} · ${entry.event}${entry.range_start != null ? ` · bytes ${entry.range_start}–${entry.range_end ?? 0}` : ''}</li>`}<//></ul>
    </section><//>
  <//>`;
}
function TransferRow(props) {
  // For is keyed by the immutable transfer ID. Progress changes only these bindings,
  // so a pointer-down / pointer-up pair always lands on the same button element.
  const job = () => jobs().find(row => row.key === props.transferKey) || {};
  const canResume = () => !job().orphan && !job().complete && (job().kind === 'download' || job().file);
  const blocked = () => job().pausing || job().removing || (!job().active && job().busyElsewhere);
  const resumeUpload = file => {
    const current = job();
    return upload(file, current.name, current.scope, current.uploadId ? { id: current.uploadId, size: current.size, sha256: current.sha256 } : undefined);
  };
  return html`<article class="card card-body mb-2">
    <div class="d-flex flex-wrap justify-content-between gap-2"><strong class="filename">${() => job().name}</strong>
      <span class="small">${() => job().orphan ? 'Leftover data' : `${job().kind === 'upload' ? 'Upload' : 'Download'} · ${bytes(job().offset || 0)} / ${bytes(job().size)}`}</span></div>
    <progress hidden=${() => job().orphan} class="my-2 w-100" max=${() => Math.max(1, job().size || 0)} value=${() => job().offset || 0} aria-label=${() => `${job().name} progress`}></progress>
    <p class="small mb-2" role="status">${() => job().busyElsewhere && !job().active ? 'In use in another tab. Pause it there before resuming or removing it here.' : job().status}</p>
    <p class="small text-body-secondary mb-2" hidden=${() => job().kind !== 'download'}>${() => {
      const row = job();
      if (row.storedBytes == null) return 'Checking browser storage…';
      return `${bytes(row.storedBytes)} stored in this browser${row.storageUnknown ? ' + files currently in use' : ''} · ${(row.entries || []).length} local files, including metadata and checkpoints`;
    }}</p>
    <div class="d-flex gap-2 flex-wrap align-items-center">
      <button class="btn btn-sm btn-outline-primary" hidden=${() => !job().active && !canResume()} disabled=${blocked} onClick=${() => {
        const current = job();
        if (current.active) pause(current.key);
        else run(() => current.kind === 'download' ? resumeDownload(current) : resumeUpload(current.file));
      }}>${() => job().pausing ? 'Pausing…' : job().active ? 'Pause' : job().kind === 'download' ? 'Resume / verify' : 'Resume'}</button>
      <label class="btn btn-sm btn-outline-primary mb-0" hidden=${() => job().kind !== 'upload' || job().active || job().complete || !!job().file}>
        Choose original file to resume<input class="visually-hidden" type="file" disabled=${blocked} onChange=${e => {
          const file = e.currentTarget.files[0]; e.currentTarget.value = ''; if (file) run(() => resumeUpload(file));
        }}/>
      </label>
      <button class="btn btn-sm btn-primary" hidden=${() => !job().complete || job().kind !== 'download'} disabled=${blocked} onClick=${() => run(() => saveDownload(job()))}>Save a copy</button>
      <button class="btn btn-sm btn-outline-secondary" hidden=${() => job().active} disabled=${blocked} onClick=${() => run(() => discard(job()))}>${() => job().removing ? 'Removing…' : job().kind === 'download' ? 'Remove from browser' : job().complete ? 'Dismiss' : 'Cancel upload'}</button>
    </div>
  </article>`;
}
function Transfers(props) {
  return html`<section aria-labelledby="transfers-title" class="mb-4">
    <h2 class="h5" id="transfers-title">Uploads and transfer controls</h2>
    <details class="mb-3"><summary>Speed limits</summary><div class="row g-3 my-1">
      ${['upload', 'download'].map(kind => html`<div class="col-sm-6"><label class="form-label" for=${`${kind}-speed`}>${kind === 'upload' ? 'Upload' : 'Download'} limit (KiB/s; 0 = unlimited)</label><input id=${`${kind}-speed`} class="form-control" type="number" min="0" value="0" onInput=${e => setRate(kind, e.currentTarget.value)}/></div>`)}
    </div><p class="small text-body-secondary">Limits apply per transfer, averaged over chunks.</p></details>
    <p class="small text-body-secondary">Uploads read your original file and keep partial bytes on the server. They do not create a copy in browser storage.</p>
    <${For} each=${() => jobs().filter(job => job.kind === 'upload' && job.scope === props.scope).map(job => job.key)} fallback=${html`<p class="text-body-secondary">No uploads.</p>`}>
      ${key => html`<${TransferRow} transferKey=${key}/>`}
    <//>
  </section>`;
}
function BrowserStorage() {
  const [confirmClear, setConfirmClear] = createSignal(false);
  const [clearing, setClearing] = createSignal(false);
  const downloads = () => jobs().filter(job => job.kind === 'download');
  const total = () => downloads().reduce((sum, job) => sum + (job.storedBytes || 0), 0);
  const unknown = () => downloads().some(job => job.storageUnknown || job.storedBytes == null);
  const active = () => downloads().some(job => job.active || job.busyElsewhere || job.removing);
  onMount(() => {
    refreshBrowserStorage();
    const refresh = () => { if (!document.hidden && !browserStorage().checking && !clearing()) refreshBrowserStorage(); };
    const timer = setInterval(refresh, 3000);
    window.addEventListener('focus', refresh);
    onCleanup(() => { clearInterval(timer); window.removeEventListener('focus', refresh); });
  });
  return html`<section aria-labelledby="browser-storage-title" class="mb-4">
    <div class="d-flex justify-content-between align-items-center flex-wrap gap-2 mb-2">
      <h2 id="browser-storage-title" class="h5 mb-0">Downloads stored in this browser</h2>
      <div class="d-flex flex-wrap gap-2">
        <button class="btn btn-sm btn-outline-secondary" disabled=${() => browserStorage().checking || clearing()} onClick=${() => refreshBrowserStorage()}>Refresh storage</button>
        <button class="btn btn-sm btn-outline-danger" disabled=${() => active() || clearing() || browserStorage().checking || (!downloads().length && !browserStorage().error)} onClick=${() => setConfirmClear(true)}>Clear browser downloads</button>
      </div>
    </div>
    <p class="small text-body-secondary">All downloads kept by this Amalthea site in this browser profile, including other accounts, sharing links, and leftover files. Saving a copy does not remove the browser copy; let the save finish before removing it. “Remove from browser” deletes its data, metadata, and checkpoints; your saved copies and server files stay available.</p>
    <p class="small" role="status">${() => {
      const state = browserStorage();
      if (state.error) return `Could not fully inspect browser storage: ${state.error} Storage may still contain files.`;
      if (!state.checked) return 'Checking browser storage…';
      if (!downloads().length) return 'Browser storage is empty: no Amalthea downloads or leftover transfer files.';
      return `${downloads().length} entries · ${bytes(total())}${unknown() ? ' + files currently in use (size available when paused)' : ''} in browser storage`;
    }}</p>
    <p class="small text-body-secondary" hidden=${() => !browserStorage().checkedAt}>Last checked: ${() => browserStorage().checkedAt ? new Date(browserStorage().checkedAt).toLocaleTimeString() : ''}</p>
    <${For} each=${() => downloads().map(job => job.key)}>${key => html`<${TransferRow} transferKey=${key}/>`}<//>
    <details class="small text-body-secondary"><summary>About browser storage</summary><p class="mt-2">Downloads need space for the whole file here, plus room for a saved copy. Private browsing, clearing site data, or browser cleanup can erase partial downloads. This list covers this Amalthea site's transfer storage; browser caches, login cookies, and other applications are separate.</p></details>
    <${Show} when=${confirmClear}>${_ => html`<${Modal} title="Clear all browser downloads?" close=${_event => { if (!clearing()) setConfirmClear(false); }}>
      <p>This removes all locally stored downloads and leftover transfer files for this Amalthea site, including other accounts and sharing links. Partial downloads will need to start again. Server files and copies you already saved are kept.</p>
      <button class="btn btn-danger" disabled=${clearing} onClick=${() => { setClearing(true); run(async () => { await clearBrowserStorage(); setConfirmClear(false); }).finally(() => setClearing(false)); }}>${() => clearing() ? 'Clearing…' : 'Clear all browser downloads'}</button>
    <//>`}<//>
  </section>`;
}
render(App, document.getElementById('app'));
