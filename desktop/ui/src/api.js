const KEY_STORAGE = "omniroute_key";

export function loadKey() {
  try {
    return localStorage.getItem(KEY_STORAGE) ?? "";
  } catch {
    return "";
  }
}

export function saveKey(value) {
  try {
    if (value) localStorage.setItem(KEY_STORAGE, value);
    else localStorage.removeItem(KEY_STORAGE);
  } catch {
    /* private mode: the key lives for this session only */
  }
}

/** Gateway base URL.
 *
 * Same origin by default (the gateway serves this bundle itself). Inside the
 * Tauri shell there is no same origin — the bundle runs on a custom scheme
 * while the API lives on loopback — so it points at the gateway directly.
 * Precedence: build-time `VITE_GATEWAY_URL`, manual `omniroute_gateway_url`
 * in localStorage (for custom ports), Tauri detection, same origin.
 */
export function apiBase() {
  const built = import.meta.env.VITE_GATEWAY_URL;
  if (built) return built.replace(/\/$/, "");
  try {
    const override = localStorage.getItem("omniroute_gateway_url");
    if (override && override.trim()) return override.trim().replace(/\/$/, "");
  } catch {
    /* private mode: no overrides */
  }
  if (typeof window !== "undefined" && window.__TAURI_INTERNALS__) {
    return "http://127.0.0.1:20128";
  }
  return "";
}

export class ApiError extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
}

async function request(path, key, init = {}) {
  const headers = { ...(init.headers ?? {}) };
  if (key) headers.authorization = `Bearer ${key}`;
  const res = await fetch(`${apiBase()}${path}`, { ...init, headers });
  if (!res.ok) {
    let detail = "";
    try {
      detail = await res.text();
    } catch {
      /* body already consumed */
    }
    throw new ApiError(res.status, detail.slice(0, 240) || `HTTP ${res.status}`);
  }
  return res.json();
}

export const getJson = (path, key) => request(path, key);

export const postJson = (path, key, body) =>
  request(path, key, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });

export const patchJson = (path, key, body) =>
  request(path, key, {
    method: "PATCH",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
