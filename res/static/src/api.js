export const base = document.querySelector('meta[name="amalthea-base"]').content;
export const endpoint = path => `${base}/api/live${path}`;
export async function api(path, options = {}) {
  const headers = { 'X-Amalthea': '1', ...options.headers };
  if (options.body && typeof options.body === 'string') headers['Content-Type'] = 'application/json';
  const response = await fetch(endpoint(path), { ...options, headers, credentials: 'same-origin', cache: 'no-store' });
  if (!response.ok) {
    const body = await response.json().catch(() => ({}));
    const error = new Error(body.error || `Request failed (${response.status})`);
    error.status = response.status;
    throw error;
  }
  return response.status === 204 ? null : response.json();
}
export const json = (method, data) => ({ method, body: JSON.stringify(data) });
export function bytes(size) {
  if (!size) return '0 B';
  const i = Math.min(4, Math.floor(Math.log(size) / Math.log(1024)));
  return `${(size / 1024 ** i).toLocaleString(undefined, { maximumFractionDigits: i ? 1 : 0 })} ${['B', 'KiB', 'MiB', 'GiB', 'TiB'][i]}`;
}
