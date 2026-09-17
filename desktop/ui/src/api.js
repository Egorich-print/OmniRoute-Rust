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

/** Gateway base URL: same origin in production, `VITE_GATEWAY_URL` in dev. */
export function apiBase() {
  return import.meta.env.VITE_GATEWAY_URL ?? "";
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
