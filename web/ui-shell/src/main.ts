import "./styles.css";

import { createSession, getSession, openSessionStream, postUiEvent } from "./api";
import type { JsonValue, UiNode, UiPropValue, UiSessionSnapshot, UiSpec } from "./types";

const appRoot = document.querySelector<HTMLDivElement>("#app");

if (!appRoot) {
  throw new Error("app root was not found");
}

const app = appRoot;

type AppState = {
  serverBaseUrl: string;
  sessionId: string;
  snapshot: UiSessionSnapshot | null;
  specFileName: string;
  specText: string;
  statusMessage: string;
  eventLog: string[];
  stream: EventSource | null;
};

const state: AppState = {
  serverBaseUrl: "http://127.0.0.1:3000",
  sessionId: "",
  snapshot: null,
  specFileName: "",
  specText: "",
  statusMessage: "Load a UiSpec JSON file to create a session.",
  eventLog: [],
  stream: null,
};

renderApp();

function renderApp(): void {
  app.innerHTML = "";

  const shell = document.createElement("div");
  shell.className = "shell";

  shell.append(createHeader());
  shell.append(createSidebar());
  shell.append(createWorkspace());

  app.append(shell);
}

function createHeader(): HTMLElement {
  const header = document.createElement("header");
  header.className = "topbar";

  const title = document.createElement("div");
  title.className = "topbar__title";
  title.innerHTML = "<strong>Pera UI Shell</strong><span>Vite + TypeScript renderer for UiSpec over REST/SSE</span>";

  const status = document.createElement("div");
  status.className = "topbar__status";
  status.textContent = state.statusMessage;

  header.append(title, status);
  return header;
}

function createSidebar(): HTMLElement {
  const sidebar = document.createElement("aside");
  sidebar.className = "sidebar";

  const configSection = document.createElement("section");
  configSection.className = "panel";
  configSection.append(sectionTitle("Session"));

  const serverLabel = document.createElement("label");
  serverLabel.className = "field";
  serverLabel.innerHTML = "<span>Server URL</span>";
  const serverInput = document.createElement("input");
  serverInput.value = state.serverBaseUrl;
  serverInput.placeholder = "http://127.0.0.1:3000";
  serverInput.addEventListener("change", () => {
    state.serverBaseUrl = serverInput.value.trim();
    setStatus(`Server URL set to ${state.serverBaseUrl}`);
  });
  serverLabel.append(serverInput);

  const fileLabel = document.createElement("label");
  fileLabel.className = "field";
  fileLabel.innerHTML = `<span>UiSpec JSON${state.specFileName ? `: ${state.specFileName}` : ""}</span>`;
  const fileInput = document.createElement("input");
  fileInput.type = "file";
  fileInput.accept = "application/json";
  fileInput.addEventListener("change", async () => {
    const file = fileInput.files?.[0];
    if (!file) {
      return;
    }
    state.specFileName = file.name;
    state.specText = await file.text();
    setStatus(`Loaded ${file.name}`);
    renderApp();
  });
  fileLabel.append(fileInput);

  const actions = document.createElement("div");
  actions.className = "button-row";
  actions.append(
    button("Create Session", async () => {
      if (!state.specText) {
        setStatus("Load a UiSpec JSON file first.");
        return;
      }
      const spec = JSON.parse(state.specText) as UiSpec;
      const snapshot = await createSession(state.serverBaseUrl, spec);
      attachSnapshot(snapshot);
      setStatus(`Created session ${snapshot.session_id}`);
    }),
    button("Refresh", async () => {
      if (!state.sessionId) {
        setStatus("No active session.");
        return;
      }
      const snapshot = await getSession(state.serverBaseUrl, state.sessionId);
      attachSnapshot(snapshot);
      setStatus(`Fetched session ${snapshot.session_id}`);
    }),
  );

  configSection.append(serverLabel, fileLabel, actions);

  const sessionInfo = document.createElement("section");
  sessionInfo.className = "panel";
  sessionInfo.append(sectionTitle("Live Data"));

  const sessionId = document.createElement("div");
  sessionId.className = "key-value";
  sessionId.innerHTML = `<span>Session</span><code>${state.sessionId || "none"}</code>`;

  const status = document.createElement("div");
  status.className = "key-value";
  status.innerHTML = `<span>Status</span><code>${state.snapshot?.status ?? "idle"}</code>`;

  const streamRow = document.createElement("div");
  streamRow.className = "button-row";
  streamRow.append(
    button("Connect Stream", () => {
      if (!state.sessionId) {
        setStatus("Create or load a session first.");
        return;
      }
      state.stream?.close();
      state.stream = openSessionStream(
        state.serverBaseUrl,
        state.sessionId,
        (snapshot) => {
          attachSnapshot(snapshot);
          appendEventLog(`snapshot:${snapshot.status}`);
        },
        (eventName, payload) => {
          appendEventLog(`${eventName}: ${JSON.stringify(payload)}`);
        },
      );
      setStatus(`Connected SSE stream for ${state.sessionId}`);
    }),
    button("Disconnect", () => {
      state.stream?.close();
      state.stream = null;
      setStatus("Disconnected SSE stream");
    }),
  );

  sessionInfo.append(sessionId, status, streamRow);

  const logSection = document.createElement("section");
  logSection.className = "panel panel--grow";
  logSection.append(sectionTitle("Events"));

  const log = document.createElement("pre");
  log.className = "event-log";
  log.textContent = state.eventLog.length > 0 ? state.eventLog.join("\n") : "No events yet.";
  logSection.append(log);

  sidebar.append(configSection, sessionInfo, logSection);
  return sidebar;
}

function createWorkspace(): HTMLElement {
  const workspace = document.createElement("main");
  workspace.className = "workspace";

  const surface = document.createElement("section");
  surface.className = "panel panel--surface";
  surface.append(sectionTitle("Rendered UI"));

  if (!state.snapshot) {
    const empty = document.createElement("div");
    empty.className = "empty";
    empty.textContent = "No session loaded.";
    surface.append(empty);
  } else {
    surface.append(renderNode(state.snapshot.spec.root));
  }

  const statePanel = document.createElement("section");
  statePanel.className = "panel";
  statePanel.append(sectionTitle("State Snapshot"));

  const statePre = document.createElement("pre");
  statePre.className = "json-view";
  statePre.textContent = state.snapshot
    ? JSON.stringify(state.snapshot.state, null, 2)
    : "{}";
  statePanel.append(statePre);

  workspace.append(surface, statePanel);
  return workspace;
}

function renderNode(nodeId: string): HTMLElement {
  const snapshot = state.snapshot;
  if (!snapshot) {
    const missing = document.createElement("div");
    missing.textContent = "No snapshot";
    return missing;
  }

  const node = snapshot.spec.nodes.find((entry) => entry.id === nodeId);
  if (!node) {
    const missing = document.createElement("div");
    missing.className = "empty";
    missing.textContent = `Missing node ${nodeId}`;
    return missing;
  }

  const children = node.children.map(renderNode);

  switch (node.component) {
    case "column": {
      const element = document.createElement("div");
      element.className = "ui-column";
      children.forEach((child) => element.append(child));
      return element;
    }
    case "text": {
      const element = document.createElement("div");
      element.className = "ui-text";
      element.textContent = readStringProp(node, "text");
      return element;
    }
    case "text_field": {
      const wrapper = document.createElement("label");
      wrapper.className = "ui-field";

      const label = document.createElement("span");
      label.textContent = readStringProp(node, "label");

      const input = document.createElement("input");
      input.value = readStringProp(node, "value");
      input.placeholder = node.id;
      input.addEventListener("change", async () => {
        if (!state.sessionId) {
          setStatus("No active session.");
          return;
        }
        const snapshot = await postUiEvent(state.serverBaseUrl, state.sessionId, {
          event_type: "set_value",
          payload: {
            node_id: node.id,
            value: input.value,
          },
        });
        attachSnapshot(snapshot);
        setStatus(`Updated ${node.id}`);
      });

      wrapper.append(label, input);
      return wrapper;
    }
    case "button": {
      const element = document.createElement("button");
      element.className = "ui-button";
      element.textContent = readStringProp(node, "label");
      element.addEventListener("click", async () => {
        if (!state.sessionId) {
          setStatus("No active session.");
          return;
        }
        const snapshot = await postUiEvent(state.serverBaseUrl, state.sessionId, {
          event_type: "trigger_event",
          payload: {
            node_id: node.id,
            event: "click",
          },
        });
        attachSnapshot(snapshot);
        setStatus(`Triggered click on ${node.id}`);
      });
      return element;
    }
    default: {
      const element = document.createElement("section");
      element.className = "ui-unknown";
      const title = document.createElement("strong");
      title.textContent = `${node.component} (${node.id})`;
      element.append(title);
      children.forEach((child) => element.append(child));
      return element;
    }
  }
}

function readStringProp(node: UiNode, propName: string): string {
  const value = node.props[propName];
  const resolved = resolvePropValue(value, state.snapshot?.state ?? null);
  return typeof resolved === "string" ? resolved : resolved == null ? "" : String(resolved);
}

function resolvePropValue(prop: UiPropValue | undefined, source: JsonValue): JsonValue {
  if (!prop) {
    return null;
  }
  switch (prop.kind) {
    case "null":
      return null;
    case "bool":
    case "int":
    case "string":
      return prop.value;
    case "binding":
      return getJsonPath(source, prop.binding.path);
    case "list":
      return prop.items.map((item) => resolvePropValue(item, source));
    case "object": {
      const object: Record<string, JsonValue> = {};
      for (const [key, value] of Object.entries(prop.fields)) {
        object[key] = resolvePropValue(value, source);
      }
      return object;
    }
  }
}

function getJsonPath(source: JsonValue, path: string): JsonValue {
  if (!path.startsWith("/")) {
    return null;
  }
  const parts = path
    .split("/")
    .slice(1)
    .filter((segment) => segment.length > 0);
  let current: JsonValue = source;
  for (const part of parts) {
    if (!isJsonObject(current) || !(part in current)) {
      return null;
    }
    current = current[part];
  }
  return current;
}

function isJsonObject(value: JsonValue): value is { [key: string]: JsonValue } {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function attachSnapshot(snapshot: UiSessionSnapshot): void {
  state.snapshot = snapshot;
  state.sessionId = snapshot.session_id;
  renderApp();
}

function appendEventLog(message: string): void {
  state.eventLog = [`${new Date().toLocaleTimeString()} ${message}`, ...state.eventLog].slice(0, 40);
  renderApp();
}

function setStatus(message: string): void {
  state.statusMessage = message;
  renderApp();
}

function button(label: string, onClick: () => void): HTMLButtonElement {
  const element = document.createElement("button");
  element.className = "button";
  element.textContent = label;
  element.addEventListener("click", () => {
    void Promise.resolve(onClick()).catch((error: unknown) => {
      setStatus(error instanceof Error ? error.message : String(error));
    });
  });
  return element;
}

function sectionTitle(value: string): HTMLElement {
  const title = document.createElement("h2");
  title.className = "panel__title";
  title.textContent = value;
  return title;
}
