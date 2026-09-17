import { afterEach, beforeEach, expect, mock, test, vi } from "bun:test";

const requests: unknown[] = [];
const activeDisposers: Array<() => void> = [];
const requestWaiters: Array<() => void> = [];
let importCounter = 0;
let acknowledgement = "ok";
let accepted = true;
let reportedSessionID: unknown;
let readbacks = 0;
let inFlight = 0;
let promptReady = false;

mock.module("node:net", () => ({
  default: {
    createConnection(_path: string, onConnect: () => void) {
      const handlers = new Map<string, (data?: unknown) => void>();
      let destroyed = false;
      inFlight += 1;
      const client = {
        write(input: string) {
          const request = JSON.parse(input.trim());
          if (request.method === "agent.get") {
            queueMicrotask(() => client.emit("data", `${JSON.stringify({ id: request.id, result: {
              type: "agent_info", agent: { pane_id: "test:p1", agent: "opencode",
                launch_pending: !promptReady, interactive_ready: promptReady },
            } })}\n`));
            return;
          }
          if (request.method === "pane.get") {
            readbacks += 1;
            queueMicrotask(() => client.emit("data", `${JSON.stringify({ id: request.id, result: {
              type: "pane_info", pane: { pane_id: "test:p1", agent_session: {
                source: "herdr:opencode", agent: "opencode", kind: "id",
                value: accepted ? reportedSessionID : "previous-session",
              } },
            } })}\n`));
            return;
          }
          if (request.method === "pane.report_agent_session") {
            reportedSessionID = request.params.agent_session_id;
          }
          requests.push(request);
          if (request.params.source === "herdr:opencode:ready") promptReady = true;
          requestWaiters.shift()?.();
          queueMicrotask(() => {
            if (["close", "end", "error"].includes(acknowledgement)) {
              client.emit(acknowledgement);
            } else if (acknowledgement === "timeout") {
              // The production wall-clock deadline owns termination.
            } else {
              const response = acknowledgement === "rejected"
                ? { id: request.id, error: { code: "pane_not_found" } }
                : { id: acknowledgement === "wrong-id" ? "another-request" : request.id,
                    result: { type: "ok" } };
              const data = `${JSON.stringify(response)}\n`;
              client.emit("data", data.slice(0, 7));
              client.emit("data", data.slice(7));
              if (acknowledgement === "wrong-id") client.emit("end");
            }
          });
        },
        setTimeout() {},
        on(event: string, handler: (data?: unknown) => void) {
          handlers.set(event, handler);
        },
        destroy() {
          if (!destroyed) inFlight -= 1;
          destroyed = true;
        },
        emit(event: string, data?: unknown) {
          if (!destroyed) handlers.get(event)?.(data);
        },
      };
      queueMicrotask(onConnect);
      return client;
    },
  },
}));

beforeEach(() => {
  requests.length = 0;
  requestWaiters.length = 0;
  acknowledgement = "ok";
  accepted = true;
  readbacks = 0;
  promptReady = false;
  process.env.HERDR_ENV = "1";
  process.env.HERDR_SOCKET_PATH = "test.sock";
  process.env.HERDR_PANE_ID = "test:p1";
  delete process.env.HERDR_OPENCODE_SUBAGENT_SESSION_ID;
});

afterEach(() => {
  for (const dispose of activeDisposers.splice(0)) {
    dispose();
  }
});

async function loadPlugin() {
  importCounter += 1;
  const module = await import(`./herdr-tui-session.js?test=${importCounter}`);
  return module.default;
}

function fakeApi() {
  const sessions = new Map<string, { id: string; parentID?: string; title?: string }>();
  const statuses = new Map<string, { type: string }>();
  const questions = new Set<string>();
  let current: { name: string; params?: { sessionID: string } } = { name: "home" };
  let dispose: (() => void) | undefined;
  const listeners = new Map<string, (event: unknown) => void>();
  activeDisposers.push(() => dispose?.());

  return {
    api: {
      renderer: { currentFocusedRenderable: undefined as undefined | {
        focused: boolean; isDestroyed: boolean; traits: { owner: string; role: string };
      } },
      ui: { dialog: { open: false } },
      route: {
        get current() {
          return current;
        },
      },
      state: {
        ready: true,
        session: {
          get(sessionID: string) {
            return sessions.get(sessionID);
          },
          status: (sessionID: string) => statuses.get(sessionID),
          permission: () => [],
          question: (sessionID: string) => questions.has(sessionID) ? [{ id: "question" }] : [],
        },
      },
      lifecycle: {
        onDispose(handler: () => void) {
          dispose = handler;
          return () => {};
        },
      },
      event: {
        on(type: string, handler: (event: unknown) => void) {
          listeners.set(type, handler);
          return () => listeners.delete(type);
        },
      },
    },
    addSession(session: { id: string; parentID?: string; title?: string }) {
      sessions.set(session.id, session);
    },
    setStatus(sessionID: string, type: string) {
      statuses.set(sessionID, { type });
    },
    setQuestion(sessionID: string, blocked: boolean) {
      if (blocked) questions.add(sessionID); else questions.delete(sessionID);
    },
    select(sessionID: string) {
      current = { name: "session", params: { sessionID } };
    },
    dispose() {
      dispose?.();
    },
    emit(type: string, sessionID?: string) {
      listeners.get(type)?.({ type, properties: { sessionID } });
    },
  };
}

function waitForNextRequest(): Promise<void> {
  return new Promise((resolve) => requestWaiters.push(resolve));
}

test("opencode task titles follow the selected root and same-session title changes", async () => {
  const plugin = await loadPlugin();
  const tui = fakeApi();
  tui.addSession({ id: "root", title: "Erste Aufgabe" });
  tui.addSession({ id: "child", parentID: "root", title: "Analyse" });
  tui.select("root");
  await plugin.tui(tui.api);
  const titles = () => requests.filter((request: any) => request.method === "pane.report_metadata");
  expect(requestParam(titles()[0], "title")).toBe("Erste Aufgabe");
  tui.addSession({ id: "root", title: "Nächste Aufgabe" });
  await new Promise((resolve) => setTimeout(resolve, 150));
  expect(requestParam(titles().at(-1), "title")).toBe("Nächste Aufgabe");
  tui.select("child");
  await new Promise((resolve) => setTimeout(resolve, 150));
  expect(titles()).toHaveLength(2);
  tui.select("root");
  tui.addSession({ id: "root", title: "" });
  await new Promise((resolve) => setTimeout(resolve, 150));
  expect(requestParam(titles().at(-1), "clear_title")).toBe(true);
});

test("opencode task titles in attached child panes use only their bound session", async () => {
  process.env.HERDR_OPENCODE_SUBAGENT_SESSION_ID = "child";
  const plugin = await loadPlugin();
  const tui = fakeApi();
  tui.addSession({ id: "root", title: "Root" });
  tui.addSession({ id: "child", parentID: "root", title: "Analyse" });
  tui.select("child");
  await plugin.tui(tui.api);
  const titles = () => requests.filter((request: any) => request.method === "pane.report_metadata");
  expect(titles()).toHaveLength(1);
  expect(requestParam(titles()[0], "title")).toBe("Analyse");
  expect(requestParam(titles()[0], "source")).toBe("herdr:opencode:title");
  tui.select("root");
  await new Promise((resolve) => setTimeout(resolve, 150));
  expect(titles()).toHaveLength(1);
});

test("reports a root session when only the local route changes", async () => {
  const plugin = await loadPlugin();
  const tui = fakeApi();
  tui.addSession({ id: "session-a" });
  await plugin.tui(tui.api);

  const dispatched = waitForNextRequest();
  tui.select("session-a");
  await dispatched;

  expect(requests).toHaveLength(1);
  expect(requestParam(requests[0], "agent_session_id")).toBe("session-a");
  expect(requestParam(requests[0], "session_start_source")).toBe("select");
  expect(requestParam(requests[0], "seq")).toBeUndefined();
});

test("managed startup reports readiness only for a mounted focused OpenCode prompt", async () => {
  const plugin = await loadPlugin();
  const tui = fakeApi();
  await plugin.tui(tui.api);
  expect(requests).toHaveLength(0);
  tui.api.renderer.currentFocusedRenderable = {
    focused: true, isDestroyed: false, traits: { owner: "other", role: "prompt" },
  };
  await new Promise((resolve) => setTimeout(resolve, 125));
  expect(requests).toHaveLength(0);
  tui.api.renderer.currentFocusedRenderable.traits.owner = "opencode";
  tui.api.ui.dialog.open = true;
  await new Promise((resolve) => setTimeout(resolve, 125));
  expect(requests).toHaveLength(0);
  tui.api.ui.dialog.open = false;
  await new Promise((resolve) => setTimeout(resolve, 125));
  expect(requests).toHaveLength(1);
  expect(requestParam(requests[0], "source")).toBe("herdr:opencode:ready");
  expect(requestParam(requests[0], "agent_session_id")).toBeUndefined();
  expect(requestParam(requests[0], "suppress_completion")).toBe(true);
  await new Promise((resolve) => setTimeout(resolve, 125));
  expect(requests).toHaveLength(1);
});

test("receipt alone retries until the selected session is read back", async () => {
  accepted = false;
  const plugin = await loadPlugin();
  const tui = fakeApi();
  tui.addSession({ id: "session-a" });
  tui.select("session-a");

  await plugin.tui(tui.api);
  expect(readbacks).toBe(1);
  accepted = true;
  await new Promise((resolve) => setTimeout(resolve, 150));
  await new Promise((resolve) => setTimeout(resolve, 150));

  expect(requests.map((request) => requestParam(request, "agent_session_id"))).toEqual([
    "session-a",
    "session-a",
  ]);
  expect(readbacks).toBe(2);
});

test("does not report root sessions not selected by this TUI", async () => {
  const plugin = await loadPlugin();
  const tui = fakeApi();
  tui.addSession({ id: "session-a" });
  tui.addSession({ id: "session-b" });
  tui.select("session-a");
  await plugin.tui(tui.api);

  await new Promise((resolve) => setTimeout(resolve, 125));

  expect(requests.length).toBeGreaterThan(0);
  expect(requests.every((request) => requestParam(request, "agent_session_id") === "session-a")).toBe(
    true,
  );
});

test("does not replace the root session with a selected child session", async () => {
  const plugin = await loadPlugin();
  const tui = fakeApi();
  tui.addSession({ id: "root-session" });
  tui.addSession({ id: "child-session", parentID: "root-session" });
  tui.select("root-session");
  await plugin.tui(tui.api);
  expect(requests).toHaveLength(1);

  tui.select("child-session");
  await new Promise((resolve) => setTimeout(resolve, 125));

  expect(requests).toHaveLength(1);
  expect(requestParam(requests[0], "agent_session_id")).toBe("root-session");
});

test("attached TUI reports existing child activity only after selection is accepted", async () => {
  process.env.HERDR_OPENCODE_SUBAGENT_SESSION_ID = "child-session";
  accepted = false;
  const plugin = await loadPlugin();
  const tui = fakeApi();
  tui.addSession({ id: "root-session" });
  tui.addSession({ id: "child-session", parentID: "root-session" });
  tui.setStatus("child-session", "busy");
  tui.select("child-session");

  await plugin.tui(tui.api);

  expect(requests).toHaveLength(1);
  expect(requestParam(requests[0], "agent_session_id")).toBe("child-session");
  expect(requestParam(requests[0], "state")).toBeUndefined();

  accepted = true;
  await new Promise((resolve) => setTimeout(resolve, 250));
  expect(requestParam(requests.at(-1), "state")).toBe("working");
  tui.setQuestion("child-session", true);
  await new Promise((resolve) => setTimeout(resolve, 125));
  expect(requestParam(requests.at(-1), "state")).toBe("blocked");
  tui.setStatus("child-session", "idle");
  await new Promise((resolve) => setTimeout(resolve, 125));
  expect(requestParam(requests.at(-1), "state")).toBe("blocked");
  tui.setQuestion("child-session", false);
  await new Promise((resolve) => setTimeout(resolve, 125));
  expect(requestParam(requests.at(-1), "state")).toBe("idle");
  tui.setStatus("child-session", "retry");
  await new Promise((resolve) => setTimeout(resolve, 125));
  expect(requestParam(requests.at(-1), "state")).toBe("working");

  const reports = requests.filter((request) => requestParam(request, "state") !== undefined);
  expect(reports.map((request) => requestParam(request, "state"))).toEqual([
    "working", "blocked", "idle", "working",
  ]);
  expect(reports.every((request) => requestParam(request, "agent_session_id") === "child-session")).toBe(true);
  tui.select("root-session");
  tui.setStatus("root-session", "busy");
  const count = requests.length;
  await new Promise((resolve) => setTimeout(resolve, 125));
  expect(requests).toHaveLength(count);
});

test("stops route polling when the TUI plugin is disposed", async () => {
  const plugin = await loadPlugin();
  const tui = fakeApi();
  tui.addSession({ id: "session-a" });
  await plugin.tui(tui.api);
  tui.dispose();
  tui.select("session-a");

  await new Promise((resolve) => setTimeout(resolve, 125));

  expect(requests).toHaveLength(0);
});

test.each(["close", "end", "error", "wrong-id", "rejected", "timeout"])(
  "%s is not a selection acknowledgement", async (mode) => {
    acknowledgement = mode;
    const plugin = await loadPlugin();
    const tui = fakeApi();
    tui.addSession({ id: "session-a" });
    tui.select("session-a");
    await plugin.tui(tui.api);
    tui.dispose();
    expect(requests).toHaveLength(1);
    expect(readbacks).toBe(0);
    expect(inFlight).toBe(0);
  },
);

test("exhausted delivery resumes on selected-session activity, not a retry daemon", async () => {
  let now = Date.now();
  const clock = vi.spyOn(Date, "now").mockImplementation(() => now);
  try {
    acknowledgement = "rejected";
    const plugin = await loadPlugin();
    const tui = fakeApi();
    tui.addSession({ id: "session-a" });
    tui.select("session-a");
    await plugin.tui(tui.api);
    for (let attempt = 0; attempt < 3; attempt += 1) {
      now += 2_000;
      await new Promise((resolve) => setTimeout(resolve, 125));
    }
    expect(requests).toHaveLength(4);
    now += 60_000;
    tui.emit("session.status", "foreign-session");
    await new Promise((resolve) => setTimeout(resolve, 125));
    expect(requests).toHaveLength(4);
    acknowledgement = "ok";
    tui.emit("session.status", "session-a");
    await new Promise((resolve) => setTimeout(resolve, 125));
    expect(requests).toHaveLength(5);
    expect(readbacks).toBe(1);
    tui.emit("session.status", "session-a");
    await new Promise((resolve) => setTimeout(resolve, 125));
    expect(requests).toHaveLength(5);
    tui.emit("server.connected");
    await new Promise((resolve) => setTimeout(resolve, 125));
    expect(requests).toHaveLength(6);
    tui.dispose();
    expect(inFlight).toBe(0);
  } finally {
    clock.mockRestore();
  }
});

test("disposal cancels the pending selection request", async () => {
  acknowledgement = "timeout";
  const plugin = await loadPlugin();
  const tui = fakeApi();
  tui.addSession({ id: "session-a" });
  tui.select("session-a");
  const dispatched = waitForNextRequest();
  const starting = plugin.tui(tui.api);
  await dispatched;
  tui.dispose();
  await starting;
  expect(inFlight).toBe(0);
  expect(readbacks).toBe(0);
});

function requestParam(request: unknown, name: string): unknown {
  if (!isRecord(request) || !isRecord(request.params)) {
    return undefined;
  }
  return request.params[name];
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
