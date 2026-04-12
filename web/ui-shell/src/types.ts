export type JsonValue =
  | null
  | boolean
  | number
  | string
  | JsonValue[]
  | { [key: string]: JsonValue };

export interface UiBinding {
  path: string;
}

export type UiPropValue =
  | { kind: "null" }
  | { kind: "bool"; value: boolean }
  | { kind: "int"; value: number }
  | { kind: "string"; value: string }
  | { kind: "binding"; binding: UiBinding }
  | { kind: "list"; items: UiPropValue[] }
  | { kind: "object"; fields: Record<string, UiPropValue> };

export type UiEvent =
  | "click"
  | "change"
  | "submit"
  | "focus"
  | "blur"
  | "open"
  | "close"
  | "select"
  | "input";

export interface UiActionArgValue {
  kind: "literal" | "binding";
  value?: unknown;
  binding?: UiBinding;
}

export interface UiActionInvocation {
  name: string;
  args: Record<string, UiActionArgValue>;
}

export interface UiActionResultBinding {
  path: string;
}

export interface UiEventHandler {
  event: UiEvent;
  action: UiActionInvocation;
  result: UiActionResultBinding | null;
}

export interface UiNode {
  id: string;
  component: string;
  props: Record<string, UiPropValue>;
  children: string[];
  events: UiEventHandler[];
}

export interface UiSpec {
  id: string;
  version: string;
  title?: string | null;
  root: string;
  nodes: UiNode[];
  data_schema?: JsonValue;
  initial_data?: JsonValue;
}

export interface UiSessionSnapshot {
  session_id: string;
  spec: UiSpec;
  state: JsonValue;
  status: string;
}

export interface UiEventRequest {
  event_type: "render" | "set_value" | "trigger_event";
  payload: JsonValue;
}
