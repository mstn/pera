import type { JsonValue, UiEventRequest, UiSessionSnapshot, UiSpec } from "./types";

export async function createSession(
  serverBaseUrl: string,
  spec: UiSpec,
  state: JsonValue = {},
): Promise<UiSessionSnapshot> {
  const response = await fetch(`${serverBaseUrl}/ui/sessions`, {
    method: "POST",
    headers: {
      "content-type": "application/json",
    },
    body: JSON.stringify({ spec, state }),
  });
  return readJsonResponse<UiSessionSnapshot>(response);
}

export async function getSession(
  serverBaseUrl: string,
  sessionId: string,
): Promise<UiSessionSnapshot> {
  const response = await fetch(`${serverBaseUrl}/ui/sessions/${sessionId}`);
  return readJsonResponse<UiSessionSnapshot>(response);
}

export async function postUiEvent(
  serverBaseUrl: string,
  sessionId: string,
  event: UiEventRequest,
): Promise<UiSessionSnapshot> {
  const response = await fetch(`${serverBaseUrl}/ui/sessions/${sessionId}/events`, {
    method: "POST",
    headers: {
      "content-type": "application/json",
    },
    body: JSON.stringify(event),
  });
  return readJsonResponse<UiSessionSnapshot>(response);
}

export function openSessionStream(
  serverBaseUrl: string,
  sessionId: string,
  onSnapshot: (snapshot: UiSessionSnapshot) => void,
  onEvent: (eventName: string, payload: JsonValue) => void,
): EventSource {
  const source = new EventSource(`${serverBaseUrl}/ui/sessions/${sessionId}/stream`);
  source.addEventListener("snapshot", (event) => {
    if (!(event instanceof MessageEvent)) {
      return;
    }
    const snapshot = JSON.parse(event.data) as UiSessionSnapshot;
    onSnapshot(snapshot);
  });
  source.onmessage = (event) => {
    const payload = JSON.parse(event.data) as JsonValue;
    onEvent("message", payload);
  };
  source.addEventListener("ui_event_received", (event) => {
    if (!(event instanceof MessageEvent)) {
      return;
    }
    const payload = JSON.parse(event.data) as JsonValue;
    onEvent("ui_event_received", payload);
  });
  return source;
}

async function readJsonResponse<T>(response: Response): Promise<T> {
  const payload = (await response.json()) as unknown;
  if (!response.ok) {
    const message =
      typeof payload === "object" &&
      payload !== null &&
      "error" in payload &&
      typeof (payload as { error?: unknown }).error === "string"
        ? (payload as { error: string }).error
        : `Request failed with HTTP ${response.status}`;
    throw new Error(message);
  }
  return payload as T;
}
