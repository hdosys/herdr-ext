import { afterEach, beforeEach, expect, mock, test, vi } from "bun:test";
import { createHash } from "node:crypto";

const requests: unknown[] = [];
const clients: FakeClient[] = [];
const requestWaiters: Array<() => void> = [];
const methodResults = new Map<string, unknown[]>();
let autoAcknowledge = true;
let importCounter = 0;
let originalFetch: typeof globalThis.fetch;
let selectedRootSessionID: string | undefined;
let selectionAvailable = true;

type FakeClient = {
  destroyed: boolean;
  emit: (event: string, data?: unknown) => void;
};

mock.module("node:net", () => ({
  default: {
    createConnection(_path: string, onConnect: () => void) {
      const handlers = new Map<string, (data?: unknown) => void>();
      const client = {
        destroyed: false,
        write(input: string) {
          const request = JSON.parse(input.trim());
          if (request.method === "pane.get") {
            const result = selectionAvailable ? {
              type: "pane_info",
              pane: {
                pane_id: "test:p1",
                ...(selectedRootSessionID ? { agent_session: {
                  source: "herdr:opencode", agent: "opencode", kind: "id",
                  value: selectedRootSessionID,
                } } : {}),
              },
            } : { type: "ok" };
            queueMicrotask(() => client.emit("data", `${JSON.stringify({ id: request.id, result })}\n`));
            return;
          }
          clients.push(client);
          requests.push(request);
          requestWaiters.shift()?.();
          if (autoAcknowledge) {
            const results = methodResults.get(request.method);
            const result = results?.shift() ?? { type: "ok" };
            queueMicrotask(() => {
              client.emit("data", `${JSON.stringify({ id: request.id, result })}\n`);
            });
          }
        },
        setTimeout() {},
        on(event: string, handler: (data?: unknown) => void) {
          handlers.set(event, handler);
        },
        destroy() {
          client.destroyed = true;
        },
        emit(event: string, data?: unknown) {
          handlers.get(event)?.(data);
        },
      };
      queueMicrotask(onConnect);
      return client;
    },
  },
}));

beforeEach(() => {
  requests.length = 0;
  clients.length = 0;
  requestWaiters.length = 0;
  methodResults.clear();
  autoAcknowledge = true;
  selectedRootSessionID = "root-session";
  selectionAvailable = true;
  process.env.HERDR_ENV = "1";
  process.env.HERDR_SOCKET_PATH = "test.sock";
  process.env.HERDR_PANE_ID = "test:p1";
  delete process.env.HERDR_OPENCODE_SUBAGENT_SESSION_ID;
  originalFetch = globalThis.fetch;
  globalThis.fetch = async () => new Response(null, { status: 200 });
});

afterEach(() => {
  globalThis.fetch = originalFetch;
});

async function loadPluginFactory() {
  importCounter += 1;
  const { HerdrAgentStatePlugin } = await import(`./herdr-agent-state.js?test=${importCounter}`);
  return HerdrAgentStatePlugin;
}

async function loadPlugin(context?: { client?: unknown; directory?: string; serverUrl?: URL }) {
  return (await loadPluginFactory())(context);
}

function waitForNextRequest(): Promise<void> {
  return new Promise((resolve) => requestWaiters.push(resolve));
}

function enqueueResult(method: string, result: unknown) {
  const results = methodResults.get(method) ?? [];
  results.push(result);
  methodResults.set(method, results);
}

function acknowledgeRequest(
  clientIndex: number,
  requestIndex: number,
  response: Record<string, unknown> = { result: { type: "ok" } },
) {
  const request = requests[requestIndex];
  if (!isRecord(request) || typeof request.id !== "string") {
    throw new Error("missing request id");
  }
  clients[clientIndex]?.emit(
    "data",
    `${JSON.stringify({ id: request.id, ...response })}\n`,
  );
}

function sessionStatusEvent(sessionID: string, status: Record<string, unknown>) {
  return {
    event: {
      type: "session.status",
      properties: { sessionID, status },
    },
  };
}

async function openDirectChild(
  plugin: Awaited<ReturnType<typeof loadPlugin>>,
  sessionID = "child-session",
) {
  enqueueResult("pane.layout", {
    type: "pane_layout",
    layout: { panes: [{ pane_id: "test:p1", rect: { width: 200, height: 50 } }] },
  });
  enqueueResult("pane.split", { type: "pane_info", pane: { pane_id: "test:p2" } });
  enqueueResult("agent.start", {
    type: "agent_started",
    agent: { pane_id: "test:p2" },
    argv: [],
  });
  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID,
        info: { id: sessionID, parentID: "root-session" },
      },
    },
  });
}

function apiErrorEvent(sessionID?: string) {
  return {
    event: {
      type: "session.error",
      properties: {
        ...(sessionID ? { sessionID } : {}),
        error: {
          name: "APIError",
          data: {
            message: "Service unavailable",
            statusCode: 503,
            isRetryable: true,
          },
        },
      },
    },
  };
}

function abortedErrorEvent(sessionID?: string) {
  return {
    event: {
      type: "session.error",
      properties: {
        ...(sessionID ? { sessionID } : {}),
        error: {
          name: "MessageAbortedError",
          data: { message: "Aborted" },
        },
      },
    },
  };
}

test("serializes lifecycle reports", async () => {
  autoAcknowledge = false;
  const plugin = await loadPlugin();
  const firstDispatched = waitForNextRequest();
  const working = plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "root-session", status: { type: "busy" } },
    },
  });
  await firstDispatched;

  const secondDispatched = waitForNextRequest();
  const idle = plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "root-session", status: { type: "idle" } },
    },
  });
  expect(clients).toHaveLength(1);

  acknowledgeRequest(0, 0);
  await secondDispatched;
  expect(clients).toHaveLength(2);
  acknowledgeRequest(1, 1);
  await Promise.all([working, idle]);

  expect(requests.map(requestState)).toEqual(["working", "idle"]);
  const sequences = requests.map(requestSeq);
  expect(sequences[0]).toEqual(expect.any(Number));
  expect(sequences[1]).toBe((sequences[0] as number) + 1);
});

test("ignores session.updated once the current root is established", async () => {
  const plugin = await loadPlugin();

  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "root-session", status: { type: "busy" } },
    },
  });
  await plugin.event({
    event: { type: "session.updated", properties: { sessionID: "root-session" } },
  });
  await plugin.event({
    event: { type: "session.updated", properties: { sessionID: "replacement-session" } },
  });

  expect(requests.map(requestMethod)).toEqual(["pane.report_agent"]);
  expect(requests.map(requestSessionID)).toEqual(["root-session"]);
});

test("does not classify server activity in another root session as a selection", async () => {
  selectedRootSessionID = "visible-session";
  const plugin = await loadPlugin();

  await plugin["chat.message"]({ sessionID: "visible-session" });
  await plugin["chat.message"]({ sessionID: "attached-client-session" });

  expect(requests.map(requestMethod)).toEqual([
    "pane.report_agent",
  ]);
  expect(requests.map(requestSessionID)).toEqual([
    "visible-session",
  ]);
});

test("does not classify server-global root creation as a local selection", async () => {
  selectedRootSessionID = undefined;
  const plugin = await loadPlugin();

  await plugin.event({
    event: { type: "session.created", properties: { sessionID: "attached-session" } },
  });
  await plugin.event({
    event: { type: "session.updated", properties: { sessionID: "attached-session" } },
  });
  await plugin["chat.message"]({ sessionID: "attached-session" });

  expect(requests).toHaveLength(0);
  selectedRootSessionID = "attached-session";
  await plugin["chat.message"]({ sessionID: "attached-session" });

  expect(requests.map(requestMethod)).toEqual(["pane.report_agent"]);
  expect(requests.map(requestSessionID)).toEqual(["attached-session"]);
});

test("reports retry status as working", async () => {
  const plugin = await loadPlugin();

  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "root-session", status: { type: "retry" } },
    },
  });

  expect(requests.map(requestMethod)).toEqual(["pane.report_agent"]);
  expect(requests.map(requestState)).toEqual(["working"]);
  expect(requests.map(requestSessionID)).toEqual(["root-session"]);
});

test("reports a user-aborted session as idle instead of blocked", async () => {
  const plugin = await loadPlugin();

  await plugin.event(sessionStatusEvent("root-session", { type: "busy" }));
  await plugin.event(abortedErrorEvent("root-session"));

  expect(requests.map(requestState)).toEqual(["working", "idle"]);
  expect(requests.map(requestSessionID)).toEqual(["root-session", "root-session"]);
  expect(
    (requests[0] as { params: { suppress_completion?: boolean } }).params
      .suppress_completion,
  ).toBeUndefined();
  expect(
    (requests[1] as { params: { suppress_completion?: boolean } }).params
      .suppress_completion,
  ).toBe(true);
});

test("error followed by retry reports working without blocked", async () => {
  vi.useFakeTimers();
  try {
    const plugin = await loadPlugin();

    await plugin.event(apiErrorEvent("root-session"));
    expect(requests).toHaveLength(0);

    await plugin.event(
      sessionStatusEvent("root-session", {
        type: "retry",
        attempt: 1,
        message: "Service unavailable",
        next: Date.now() + 2_000,
      }),
    );
    vi.runAllTimers();
    await Promise.resolve();

    expect(requests.map(requestState)).toEqual(["working"]);
    expect(requests.map(requestSessionID)).toEqual(["root-session"]);
  } finally {
    vi.useRealTimers();
  }
});

test("retry then error then busy never reports blocked", async () => {
  vi.useFakeTimers();
  try {
    const plugin = await loadPlugin();

    await plugin.event(
      sessionStatusEvent("root-session", {
        type: "retry",
        attempt: 1,
        message: "Service unavailable",
        next: Date.now() + 30_000,
      }),
    );
    await plugin.event(apiErrorEvent("root-session"));
    vi.advanceTimersByTime(5_000);
    await Promise.resolve();
    expect(requests.map(requestState)).toEqual(["working"]);

    await plugin.event(sessionStatusEvent("root-session", { type: "busy" }));
    vi.runAllTimers();
    await Promise.resolve();

    expect(requests.map(requestState)).toEqual(["working", "working"]);
    expect(requests.map(requestState)).not.toContain("blocked");
  } finally {
    vi.useRealTimers();
  }
});

test("error then idle reports blocked once without idle overwrite", async () => {
  const plugin = await loadPlugin();

  await plugin.event(apiErrorEvent("root-session"));
  await plugin.event(sessionStatusEvent("root-session", { type: "idle" }));
  await plugin.event({
    event: { type: "session.idle", properties: { sessionID: "root-session" } },
  });

  expect(requests.map(requestState)).toEqual(["blocked"]);
  expect(requests.map(requestSessionID)).toEqual(["root-session"]);
});

test("error without follow-up reports blocked through bounded fallback", async () => {
  vi.useFakeTimers();
  try {
    const plugin = await loadPlugin();
    await plugin.event(apiErrorEvent("root-session"));
    expect(requests).toHaveLength(0);

    const dispatched = waitForNextRequest();
    vi.runAllTimers();
    await dispatched;

    expect(requests.map(requestState)).toEqual(["blocked"]);
    expect(requests.map(requestSessionID)).toEqual(["root-session"]);
  } finally {
    vi.useRealTimers();
  }
});

test("unrelated status cannot cancel another session pending error", async () => {
  selectedRootSessionID = "first-session";
  const plugin = await loadPlugin();

  await plugin.event(apiErrorEvent("first-session"));
  await plugin.event(
    sessionStatusEvent("second-session", {
      type: "retry",
      attempt: 1,
      message: "Service unavailable",
      next: Date.now() + 2_000,
    }),
  );
  await plugin.event(sessionStatusEvent("first-session", { type: "idle" }));

  expect(requests.map(requestState)).toEqual(["blocked"]);
  expect(requests.map(requestSessionID)).toEqual(["first-session"]);
});

test("session deletion clears a pending error fallback", async () => {
  vi.useFakeTimers();
  try {
    const plugin = await loadPlugin();
    await plugin.event(apiErrorEvent("root-session"));
    await plugin.event({
      event: {
        type: "session.deleted",
        properties: { info: { id: "root-session" } },
      },
    });

    vi.runAllTimers();
    await Promise.resolve();
    expect(requests).toHaveLength(0);
  } finally {
    vi.useRealTimers();
  }
});

test("new session retires old pending lifecycle and stale events", async () => {
  vi.useFakeTimers();
  try {
    const plugin = await loadPlugin();
    selectedRootSessionID = "old-session";
    await plugin.event(apiErrorEvent("old-session"));
    selectedRootSessionID = "new-session";
    await plugin["chat.message"]({ sessionID: "new-session" });

    await plugin.event(sessionStatusEvent("old-session", { type: "idle" }));
    await plugin.event(apiErrorEvent("old-session"));
    await plugin.event({
      event: {
        type: "session.idle",
        properties: { sessionID: "old-session" },
      },
    });
    vi.runAllTimers();
    await Promise.resolve();

    expect(requests.map(requestMethod)).toEqual(["pane.report_agent"]);
    expect(requests.map(requestSessionID)).toEqual(["new-session"]);
    expect(requests.map(requestState)).toEqual(["working"]);
  } finally {
    vi.useRealTimers();
  }
});

test("owned busy clears an unscoped error but not an outstanding question", async () => {
  const plugin = await loadPlugin();
  await plugin.event({
    event: {
      type: "session.created",
      properties: { sessionID: "root-session" },
    },
  });
  requests.length = 0;

  await plugin.event(apiErrorEvent());
  // Herdr clears a session when a lifecycle report omits its identity. Assert
  // the real input contract, rather than relying on pane.get's fixed fixture.
  expect(requestSessionID(requests.at(-1))).toBe("root-session");
  await plugin.event(sessionStatusEvent("foreign-root", { type: "busy" }));
  expect(requests.map(requestState)).toEqual(["blocked"]);
  await plugin.event(sessionStatusEvent("root-session", { type: "busy" }));
  expect(requests.map(requestState)).toEqual(["blocked", "working"]);
  await plugin.event({ event: { type: "question.asked", properties: {
    id: "question-1", sessionID: "root-session",
  } } });
  await plugin.event(apiErrorEvent());
  await plugin.event(sessionStatusEvent("root-session", { type: "busy" }));
  expect(requests.map(requestState)).toEqual(["blocked", "working", "blocked", "blocked"]);
  await plugin.event({ event: { type: "question.replied", properties: {
    requestID: "question-1", sessionID: "root-session",
  } } });
  expect(requests.map(requestState).at(-1)).toBe("working");
});

test("dispose cancels pending fallback and ignores later events", async () => {
  vi.useFakeTimers();
  try {
    const plugin = await loadPlugin();
    await plugin.event(apiErrorEvent("root-session"));
    await plugin.dispose();
    vi.runAllTimers();
    await Promise.resolve();
    await plugin.event(sessionStatusEvent("root-session", { type: "idle" }));

    expect(requests).toHaveLength(0);
  } finally {
    vi.useRealTimers();
  }
});

test("SDK lifecycle info IDs never establish selection and deletion releases the old root", async () => {
  selectedRootSessionID = undefined;
  const plugin = await loadPlugin();
  await plugin.event({
    event: {
      type: "session.updated",
      properties: { info: { id: "startup-root" } },
    },
  });
  await plugin.event({
    event: {
      type: "session.updated",
      properties: { info: { id: "background-session" } },
    },
  });
  expect(requests).toHaveLength(0);
  selectedRootSessionID = "startup-root";
  await plugin.event(sessionStatusEvent("startup-root", { type: "busy" }));
  await plugin.event(sessionStatusEvent("background-session", { type: "busy" }));
  await plugin.event({ event: { type: "session.deleted", properties: { info: { id: "startup-root" } } } });
  await plugin.event(sessionStatusEvent("startup-root", { type: "busy" }));
  selectedRootSessionID = "background-session";
  await plugin.event(sessionStatusEvent("background-session", { type: "busy" }));

  expect(requests.map(requestMethod)).toEqual([
    "pane.report_agent",
    "pane.report_agent",
  ]);
  expect(requests.map(requestSessionID)).toEqual(["startup-root", "background-session"]);
});

test("background session.updated cannot steal a pending terminal root", async () => {
  selectedRootSessionID = "root-a";
  const plugin = await loadPlugin();
  await plugin.event(apiErrorEvent("root-a"));
  await plugin.event({
    event: {
      type: "session.updated",
      properties: { sessionID: "root-b" },
    },
  });
  await plugin.event(sessionStatusEvent("root-a", { type: "idle" }));

  expect(requests.map(requestMethod)).toEqual(["pane.report_agent"]);
  expect(requests.map(requestState)).toEqual(["blocked"]);
  expect(requests.map(requestSessionID)).toEqual(["root-a"]);
});

test("chat.message consumes only the selection read from the pane owner", async () => {
  selectedRootSessionID = "root-a";
  const plugin = await loadPlugin();
  await plugin.event({
    event: {
      type: "session.created",
      properties: { sessionID: "root-a" },
    },
  });
  await plugin["chat.message"]({ sessionID: "root-b" });
  await plugin.event(sessionStatusEvent("root-a", { type: "busy" }));
  selectedRootSessionID = "root-b";
  await plugin.event(sessionStatusEvent("root-b", { type: "busy" }));

  expect(requests.map(requestMethod)).toEqual([
    "pane.report_agent",
    "pane.report_agent",
  ]);
  expect(requests.map(requestSessionID)).toEqual(["root-a", "root-b"]);
});

test("tiles five concurrent direct children across the complete tab", async () => {
  const plugin = await loadPlugin({
    directory: "C:\\repo",
    serverUrl: new URL("http://127.0.0.1:4096"),
  });
  await plugin["chat.message"]({ sessionID: "root-session" });
  requests.length = 0;

  let nextPaneNumber = 2;
  function openChild(sessionID: string, panes: unknown[], directory?: string) {
    const paneID = `test:p${nextPaneNumber}`;
    nextPaneNumber += 1;
    enqueueResult("pane.layout", { type: "pane_layout", layout: { panes } });
    enqueueResult("pane.split", { type: "pane_info", pane: { pane_id: paneID } });
    enqueueResult("agent.start", {
      type: "agent_started",
      agent: { pane_id: paneID },
      argv: [],
    });
    return plugin.event({
      event: {
        type: "session.created",
        properties: {
          sessionID,
          info: {
            id: sessionID,
            parentID: "root-session",
            ...(directory ? { directory } : {}),
          },
        },
      },
    });
  }

  await Promise.all([
    openChild(
      "child-one",
      [{ pane_id: "test:p1", rect: { width: 140, height: 41 } }],
      "C:\\repo\\one",
    ),
    openChild("child-two", [
      { pane_id: "test:p1", rect: { width: 70, height: 41 } },
      { pane_id: "test:p2", rect: { width: 70, height: 41 } },
    ]),
    openChild("child-three", [
      { pane_id: "test:p1", rect: { width: 70, height: 20 } },
      { pane_id: "test:p3", rect: { width: 70, height: 20 } },
      { pane_id: "test:p2", rect: { width: 70, height: 41 } },
    ]),
    openChild("child-four", [
      { pane_id: "test:p1", rect: { width: 70, height: 20 } },
      { pane_id: "test:p3", rect: { width: 70, height: 20 } },
      { pane_id: "test:p2", rect: { width: 70, height: 20 } },
      { pane_id: "test:p4", rect: { width: 70, height: 20 } },
    ]),
    openChild("child-five", [
      { pane_id: "test:p1", rect: { width: 35, height: 20 } },
      { pane_id: "test:p5", rect: { width: 35, height: 20 } },
      { pane_id: "test:p3", rect: { width: 70, height: 20 } },
      { pane_id: "test:p2", rect: { width: 70, height: 20 } },
      { pane_id: "test:p4", rect: { width: 70, height: 20 } },
    ]),
  ]);

  const splits = requests.filter((request) => requestMethod(request) === "pane.split");
  expect(splits).toHaveLength(5);
  expect(requestParam(splits[0], "target_pane_id")).toBe("test:p1");
  expect(requestParam(splits[0], "direction")).toBe("right");
  expect(requestParam(splits[0], "ratio")).toBe(0.5);
  expect(requestParam(splits[0], "focus")).toBe(false);
  expect(requestParam(splits[0], "cwd")).toBe("C:\\repo\\one");
  expect(requestParam(splits[0], "env")).toEqual({
    HERDR_OPENCODE_SUBAGENT_SESSION_ID: "child-one",
  });
  expect(requestParam(splits[1], "target_pane_id")).toBe("test:p1");
  expect(requestParam(splits[1], "direction")).toBe("down");
  expect(requestParam(splits[1], "ratio")).toBe(0.5);
  expect(requestParam(splits[2], "target_pane_id")).toBe("test:p2");
  expect(requestParam(splits[2], "direction")).toBe("down");
  expect(requestParam(splits[3], "target_pane_id")).toBe("test:p1");
  expect(requestParam(splits[3], "direction")).toBe("right");
  expect(requestParam(splits[4], "target_pane_id")).toBe("test:p3");
  expect(requestParam(splits[4], "direction")).toBe("right");

  const starts = requests.filter((request) => requestMethod(request) === "agent.start");
  expect(starts).toHaveLength(5);
  expect(requestParam(starts[0], "name")).toMatch(/^opencode-[0-9a-f]{12}$/);
  expect(requestParam(starts[0], "kind")).toBe("opencode");
  expect(requestParam(starts[0], "pane_id")).toBe("test:p2");
  expect(requestParam(starts[0], "args")).toEqual([
    "attach",
    "http://127.0.0.1:4096/",
    "--session",
    "child-one",
    "--dir",
    "C:\\repo\\one",
  ]);
  expect(requestParam(starts[0], "timeout_ms")).toBe(30_000);
  expect(requestParam(starts[0], "source")).toBeUndefined();

  requests.length = 0;
  await plugin.event(sessionStatusEvent("child-one", { type: "idle" }));
  expect(requests.map(requestMethod)).toEqual(["pane.close"]);
  expect(requestParam(requests[0], "pane_id")).toBe("test:p2");
});

test("stacks the first child when the root is not wide landscape", async () => {
  const plugin = await loadPlugin({
    directory: "C:\\repo",
    serverUrl: new URL("http://127.0.0.1:4096"),
  });
  await plugin["chat.message"]({ sessionID: "root-session" });
  requests.length = 0;

  enqueueResult("pane.layout", {
    type: "pane_layout",
    layout: {
      panes: [{ pane_id: "test:p1", rect: { width: 170, height: 100 } }],
    },
  });
  enqueueResult("pane.split", {
    type: "pane_info",
    pane: { pane_id: "test:p2" },
  });
  enqueueResult("agent.start", {
    type: "agent_started",
    agent: { pane_id: "test:p2" },
    argv: [],
  });

  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "child-session",
        info: { id: "child-session", parentID: "root-session" },
      },
    },
  });

  const split = requests.find((request) => requestMethod(request) === "pane.split");
  expect(requestParam(split, "target_pane_id")).toBe("test:p1");
  expect(requestParam(split, "direction")).toBe("down");
  expect(requestParam(split, "ratio")).toBe(0.5);
});

test("does not let a pending agent start block later pane placement", async () => {
  const plugin = await loadPlugin({
    directory: "C:\\repo",
    serverUrl: new URL("http://127.0.0.1:4096"),
  });
  await plugin["chat.message"]({ sessionID: "root-session" });
  requests.length = 0;
  clients.length = 0;
  autoAcknowledge = false;

  let dispatched = waitForNextRequest();
  const first = plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "child-one",
        info: { id: "child-one", parentID: "root-session" },
      },
    },
  });
  await dispatched;
  expect(requestMethod(requests[0])).toBe("pane.layout");

  dispatched = waitForNextRequest();
  acknowledgeRequest(0, 0, {
    result: {
      type: "pane_layout",
      layout: { panes: [{ pane_id: "test:p1", rect: { width: 200, height: 50 } }] },
    },
  });
  await dispatched;
  expect(requestMethod(requests[1])).toBe("pane.split");

  dispatched = waitForNextRequest();
  acknowledgeRequest(1, 1, {
    result: { type: "pane_info", pane: { pane_id: "test:p2" } },
  });
  await dispatched;
  expect(requestMethod(requests[2])).toBe("agent.start");

  dispatched = waitForNextRequest();
  const second = plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "child-two",
        info: { id: "child-two", parentID: "root-session" },
      },
    },
  });
  await dispatched;
  expect(requestMethod(requests[3])).toBe("pane.layout");

  dispatched = waitForNextRequest();
  acknowledgeRequest(3, 3, {
    result: {
      type: "pane_layout",
      layout: {
        panes: [
          { pane_id: "test:p1", rect: { width: 100, height: 50 } },
          { pane_id: "test:p2", rect: { width: 100, height: 50 } },
        ],
      },
    },
  });
  await dispatched;
  expect(requestMethod(requests[4])).toBe("pane.split");

  dispatched = waitForNextRequest();
  acknowledgeRequest(4, 4, {
    result: { type: "pane_info", pane: { pane_id: "test:p3" } },
  });
  await dispatched;
  expect(requestMethod(requests[5])).toBe("agent.start");

  acknowledgeRequest(2, 2, {
    result: { type: "agent_started", agent: { pane_id: "test:p2" }, argv: [] },
  });
  acknowledgeRequest(5, 5, {
    result: { type: "agent_started", agent: { pane_id: "test:p3" }, argv: [] },
  });
  await Promise.all([first, second]);

  expect(requests.map(requestMethod)).toEqual([
    "pane.layout",
    "pane.split",
    "agent.start",
    "pane.layout",
    "pane.split",
    "agent.start",
  ]);
});

test("retains a child pane when the agent start response is lost", async () => {
  const plugin = await loadPlugin({
    directory: "C:\\repo",
    serverUrl: new URL("http://127.0.0.1:4096"),
  });
  await plugin["chat.message"]({ sessionID: "root-session" });
  requests.length = 0;

  const sessionID = "child-session";
  const name = `opencode-${createHash("sha256").update(sessionID).digest("hex").slice(0, 12)}`;
  enqueueResult("pane.layout", {
    type: "pane_layout",
    layout: { panes: [{ pane_id: "test:p1", rect: { width: 200, height: 50 } }] },
  });
  enqueueResult("pane.split", { type: "pane_info", pane: { pane_id: "test:p2" } });
  enqueueResult("agent.start", { type: "ok" });
  enqueueResult("agent.get", {
    type: "agent_info",
    agent: { name, pane_id: "test:p2" },
  });

  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID,
        info: { id: sessionID, parentID: "root-session" },
      },
    },
  });

  expect(requests.map(requestMethod)).toEqual([
    "pane.layout",
    "pane.split",
    "agent.start",
    "agent.get",
  ]);
});

test("keeps the child pane when delayed idle disagrees with live status", async () => {
  let liveStatus = { type: "busy" };
  const plugin = await loadPlugin({
    client: {
      session: {
        status: async () => ({ data: { "child-session": liveStatus } }),
      },
    },
    directory: "C:\\repo",
    serverUrl: new URL("http://127.0.0.1:4096"),
  });
  await plugin["chat.message"]({ sessionID: "root-session" });
  requests.length = 0;
  await openDirectChild(plugin);

  requests.length = 0;
  await plugin.event(sessionStatusEvent("child-session", { type: "idle" }));
  expect(requests).toHaveLength(0);

  liveStatus = { type: "idle" };
  await plugin.event(sessionStatusEvent("child-session", { type: "idle" }));
  expect(requests.map(requestMethod)).toEqual(["pane.close"]);
  expect(requestParam(requests[0], "pane_id")).toBe("test:p2");
});

test("keeps the root working while a direct child is busy", async () => {
  const plugin = await loadPlugin();
  await plugin["chat.message"]({ sessionID: "root-session" });
  requests.length = 0;

  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        info: { id: "child-session", parentID: "root-session" },
      },
    },
  });
  await plugin.event(sessionStatusEvent("root-session", { type: "idle" }));
  await plugin.event(sessionStatusEvent("child-session", { type: "idle" }));

  expect(requests.map(requestState)).toEqual(["working", "idle"]);
  expect(requests.map(requestSessionID)).toEqual(["root-session", "root-session"]);
});

test("an attached child pane reports its own lifecycle", async () => {
  process.env.HERDR_OPENCODE_SUBAGENT_SESSION_ID = "child-session";
  selectedRootSessionID = "child-session";
  const plugin = await loadPlugin();

  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        info: { id: "child-session", parentID: "root-session" },
      },
    },
  });
  await plugin.event(sessionStatusEvent("child-session", { type: "busy" }));
  await plugin.event(sessionStatusEvent("child-session", { type: "idle" }));

  expect(requests.map(requestState)).toEqual(["working", "idle"]);
  expect(requests.map(requestSessionID)).toEqual(["child-session", "child-session"]);
});

test("late child work wakes an idle root and finishes without clearing a prompt", async () => {
  const plugin = await loadPlugin();
  await plugin.event(sessionStatusEvent("root-session", { type: "idle" }));
  await plugin.event({ event: { type: "session.created", properties: {
    info: { id: "child-session", parentID: "root-session" },
  } } });
  await plugin.event(sessionStatusEvent("child-session", { type: "idle" }));
  await plugin.event(sessionStatusEvent("child-session", { type: "busy" }));
  expect(requests.map(requestState)).toEqual(["idle", "working", "idle", "working"]);
  await plugin.event({ event: { type: "question.asked", properties: {
    id: "pending", sessionID: "root-session",
  } } });
  await plugin.event(sessionStatusEvent("child-session", { type: "idle" }));
  expect(requestState(requests.at(-1))).toBe("blocked");
  expect(requests.every((request) => requestSessionID(request) === "root-session")).toBe(true);
  await plugin.dispose();
});

test("reconciles child status changes while attach is starting", async () => {
  const plugin = await loadPlugin({
    directory: "C:\\repo",
    serverUrl: new URL("http://127.0.0.1:4096"),
  });
  await plugin["chat.message"]({ sessionID: "root-session" });
  requests.length = 0;
  clients.length = 0;
  autoAcknowledge = false;

  const layoutDispatched = waitForNextRequest();
  const created = plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "child-session",
        info: { id: "child-session", parentID: "root-session" },
      },
    },
  });
  await layoutDispatched;

  const splitDispatched = waitForNextRequest();
  acknowledgeRequest(0, 0, {
    result: {
      type: "pane_layout",
      layout: { panes: [{ pane_id: "test:p1", rect: { width: 200, height: 50 } }] },
    },
  });
  await splitDispatched;

  const startDispatched = waitForNextRequest();
  acknowledgeRequest(1, 1, {
    result: { type: "pane_info", pane: { pane_id: "test:p2" } },
  });
  await startDispatched;

  const idle = plugin.event(sessionStatusEvent("child-session", { type: "idle" }));
  const working = plugin.event(sessionStatusEvent("child-session", { type: "busy" }));
  acknowledgeRequest(2, 2, {
    result: {
      type: "agent_started",
      agent: { pane_id: "test:p2" },
      argv: [],
    },
  });
  await Promise.all([created, idle, working]);

  expect(requests.map(requestMethod)).toEqual([
    "pane.layout",
    "pane.split",
    "agent.start",
  ]);
});

test("dispose lets an in-flight child split report its pane before cleanup", async () => {
  const plugin = await loadPlugin({
    directory: "C:\\repo",
    serverUrl: new URL("http://127.0.0.1:4096"),
  });
  await plugin["chat.message"]({ sessionID: "root-session" });
  requests.length = 0;
  clients.length = 0;
  autoAcknowledge = false;

  const layoutDispatched = waitForNextRequest();
  const created = plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "child-session",
        info: { id: "child-session", parentID: "root-session" },
      },
    },
  });
  await layoutDispatched;
  const splitDispatched = waitForNextRequest();
  acknowledgeRequest(0, 0, {
    result: {
      type: "pane_layout",
      layout: { panes: [{ pane_id: "test:p1", rect: { width: 200, height: 50 } }] },
    },
  });
  await splitDispatched;

  const disposing = plugin.dispose();
  expect(clients[1]?.destroyed).toBe(false);
  const closeDispatched = waitForNextRequest();
  acknowledgeRequest(1, 1, {
    result: { type: "pane_info", pane: { pane_id: "test:p2" } },
  });
  await closeDispatched;
  acknowledgeRequest(2, 2);
  await Promise.all([created, disposing]);

  expect(requests.map(requestMethod)).toEqual([
    "pane.layout",
    "pane.split",
    "pane.close",
  ]);
});

test("a lost split response never adopts or closes an independently created pane", async () => {
  const plugin = await loadPlugin({
    directory: "C:\\repo",
    serverUrl: new URL("http://127.0.0.1:4096"),
  });
  await plugin["chat.message"]({ sessionID: "root-session" });
  requests.length = 0;
  clients.length = 0;
  autoAcknowledge = false;

  const layoutDispatched = waitForNextRequest();
  const created = plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "child-session",
        info: { id: "child-session", parentID: "root-session" },
      },
    },
  });
  await layoutDispatched;
  const splitDispatched = waitForNextRequest();
  acknowledgeRequest(0, 0, {
    result: {
      type: "pane_layout",
      layout: { panes: [{ pane_id: "test:p1", rect: { width: 200, height: 50 } }] },
    },
  });
  await splitDispatched;

  clients[1]?.emit("close");
  await created;
  autoAcknowledge = true;
  enqueueResult("pane.layout", { type: "pane_layout", layout: { panes: [
    { pane_id: "test:p1", rect: { width: 100, height: 50 } },
    { pane_id: "foreign:p2", rect: { width: 100, height: 50 } },
  ] } });
  await plugin.event(sessionStatusEvent("child-session", { type: "busy" }));
  await plugin.event({
    event: {
      type: "session.deleted",
      properties: {
        sessionID: "child-session",
        info: { id: "child-session", parentID: "root-session" },
      },
    },
  });
  await plugin.dispose();

  expect(requests.map(requestMethod)).toEqual([
    "pane.layout",
    "pane.split",
  ]);
});

test("dispose aborts a stalled SDK status request and closes only its known child", async () => {
  let signal: AbortSignal | undefined;
  let started!: () => void;
  const statusStarted = new Promise<void>((resolve) => { started = resolve; });
  const plugin = await loadPlugin({
    directory: "C:\\repo", serverUrl: new URL("http://127.0.0.1:4096"),
    client: { session: { status: (options: { signal: AbortSignal }) => {
      signal = options.signal;
      started();
      return new Promise(() => {});
    } } },
  });
  await openDirectChild(plugin);
  requests.length = 0;
  const idle = plugin.event(sessionStatusEvent("child-session", { type: "idle" }));
  await statusStarted;
  await plugin.dispose();
  await idle;
  expect(signal?.aborted).toBe(true);
  expect(requests.map(requestMethod)).toEqual(["pane.close"]);
  expect(requestParam(requests[0], "pane_id")).toBe("test:p2");
}, 1500);

test("a stalled SDK status times out without treating unknown state as idle", async () => {
  let signal: AbortSignal | undefined;
  const plugin = await loadPlugin({
    directory: "C:\\repo", serverUrl: new URL("http://127.0.0.1:4096"),
    client: { session: { status: (options: { signal: AbortSignal }) => {
      signal = options.signal;
      return new Promise(() => {});
    } } },
  });
  await openDirectChild(plugin);
  requests.length = 0;
  await plugin.event(sessionStatusEvent("child-session", { type: "idle" }));
  expect(signal?.aborted).toBe(true);
  expect(requests).toHaveLength(0);
  await plugin.dispose();
}, 1500);

test("A/B/A selection preserves live child prompts and ignores foreign chat", async () => {
  const plugin = await loadPlugin();
  await plugin.event({ event: { type: "session.created", properties: {
    info: { id: "child-session", parentID: "root-session" },
  } } });
  await plugin["chat.message"]({ sessionID: "foreign-root" });
  expect(requests).toHaveLength(0);
  selectedRootSessionID = "other-root";
  await plugin["chat.message"]({ sessionID: "other-root" });
  await plugin.event({ event: { type: "question.asked", properties: {
    id: "q1", sessionID: "child-session",
  } } });
  expect(requests.map(requestState)).toEqual(["working"]);
  selectedRootSessionID = "root-session";
  await plugin.event(sessionStatusEvent("root-session", { type: "busy" }));
  expect(requests.map(requestState)).toEqual(["working", "blocked"]);
  await plugin.event({ event: { type: "question.asked", properties: {
    id: "q2", sessionID: "child-session",
  } } });
  await plugin.event({ event: { type: "question.replied", properties: {
    requestID: "q1", sessionID: "child-session",
  } } });
  await plugin.event({ event: { type: "question.replied", properties: {
    requestID: "q2", sessionID: "child-session",
  } } });
  expect(requests.map(requestState)).toEqual(["working", "blocked", "blocked", "blocked", "working"]);
  expect(requests.map(requestSessionID)).toEqual(["other-root", "root-session", "root-session", "root-session", "root-session"]);
});

test("an unavailable selection never starts children or reports a foreign root", async () => {
  selectionAvailable = false;
  const plugin = await loadPlugin({ serverUrl: new URL("http://127.0.0.1:4096") });
  await plugin["chat.message"]({ sessionID: "root-session" });
  await openDirectChild(plugin);
  expect(requests).toHaveLength(0);
  await plugin.dispose();
});

test("retains pane ownership when close is not acknowledged", async () => {
  const plugin = await loadPlugin({
    directory: "C:\\repo",
    serverUrl: new URL("http://127.0.0.1:4096"),
  });
  await plugin["chat.message"]({ sessionID: "root-session" });
  await openDirectChild(plugin);

  requests.length = 0;
  clients.length = 0;
  autoAcknowledge = false;
  const firstCloseDispatched = waitForNextRequest();
  const firstIdle = plugin.event(sessionStatusEvent("child-session", { type: "idle" }));
  await firstCloseDispatched;
  const presenceDispatched = waitForNextRequest();
  acknowledgeRequest(0, 0, {
    error: { code: "server_unavailable", message: "busy" },
  });
  await presenceDispatched;
  acknowledgeRequest(1, 1, {
    result: {
      type: "pane_layout",
      layout: { panes: [{ pane_id: "test:p2", rect: { width: 100, height: 50 } }] },
    },
  });
  await firstIdle;

  const secondCloseDispatched = waitForNextRequest();
  const secondIdle = plugin.event(sessionStatusEvent("child-session", { type: "idle" }));
  await secondCloseDispatched;
  acknowledgeRequest(2, 2);
  await secondIdle;

  expect(requests.map(requestMethod)).toEqual([
    "pane.close",
    "pane.layout",
    "pane.close",
  ]);
});

test("does not split when the root OpenCode server is not externally reachable", async () => {
  globalThis.fetch = async () => {
    throw new Error("connection refused");
  };
  const plugin = await loadPlugin({
    directory: "C:\\repo",
    serverUrl: new URL("http://127.0.0.1:4096"),
  });
  await plugin["chat.message"]({ sessionID: "root-session" });
  requests.length = 0;

  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "child-session",
        info: { id: "child-session", parentID: "root-session" },
      },
    },
  });

  expect(requests).toHaveLength(0);
});

test("an accepted same-root turn clears an unscoped error block", async () => {
  const plugin = await loadPlugin();
  await plugin.event({
    event: {
      type: "session.created",
      properties: { sessionID: "root-session" },
    },
  });
  requests.length = 0;

  await plugin.event(apiErrorEvent());
  await plugin["chat.message"]({ sessionID: "root-session" });

  expect(requests.map(requestState)).toEqual(["blocked", "working"]);
  expect(requests.map(requestSessionID)).toEqual(["root-session", "root-session"]);
});

test("old child reply is dropped after root replacement", async () => {
  selectedRootSessionID = "old-root";
  const plugin = await loadPlugin();
  await plugin["chat.message"]({ sessionID: "old-root" });
  requests.length = 0;
  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "old-child",
        info: { id: "old-child", parentID: "old-root" },
      },
    },
  });
  selectedRootSessionID = "new-root";
  await plugin["chat.message"]({ sessionID: "new-root" });
  await plugin.event({
    event: {
      type: "permission.replied",
      properties: { sessionID: "old-child" },
    },
  });

  expect(requests.map(requestMethod)).toEqual(["pane.report_agent"]);
  expect(requests.map(requestSessionID)).toEqual(["new-root"]);
});

test("canonical child deletion removes ownership before trailing reply", async () => {
  const plugin = await loadPlugin();
  await plugin["chat.message"]({ sessionID: "root-session" });
  requests.length = 0;
  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "child-session",
        info: { id: "child-session", parentID: "root-session" },
      },
    },
  });
  await plugin.event({
    event: {
      type: "session.deleted",
      properties: {
        sessionID: "child-session",
        info: { id: "child-session", parentID: "root-session" },
      },
    },
  });
  await plugin.event({
    event: {
      type: "question.replied",
      properties: { sessionID: "child-session" },
    },
  });

  expect(requests).toHaveLength(0);
});

test("no-root deleted child stays retired through trailing activity", async () => {
  const plugin = await loadPlugin();
  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "child-session",
        info: { id: "child-session", parentID: "unknown-root" },
      },
    },
  });
  await plugin.event({
    event: {
      type: "session.deleted",
      properties: {
        sessionID: "child-session",
        info: { id: "child-session", parentID: "unknown-root" },
      },
    },
  });
  await plugin.event({
    event: {
      type: "question.replied",
      properties: { sessionID: "child-session" },
    },
  });
  await plugin["chat.message"]({ sessionID: "child-session" });

  expect(requests).toHaveLength(0);
});

test("initial selected root filters children learned for another parent", async () => {
  selectedRootSessionID = undefined;
  const plugin = await loadPlugin();
  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "old-child",
        info: { id: "old-child", parentID: "old-parent" },
      },
    },
  });
  selectedRootSessionID = "new-root";
  await plugin.event({ event: { type: "session.updated", properties: { info: { id: "new-root" } } } });
  await plugin.event({
    event: {
      type: "question.replied",
      properties: { sessionID: "old-child" },
    },
  });

  expect(requests).toHaveLength(0);
});

test("initial selected root ignores other children without adopting their chat", async () => {
  selectedRootSessionID = undefined;
  const plugin = await loadPlugin();
  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "old-child",
        info: { id: "old-child", parentID: "old-root" },
      },
    },
  });
  selectedRootSessionID = "new-root";
  await plugin.event(sessionStatusEvent("new-root", { type: "busy" }));
  await plugin.event({
    event: {
      type: "question.replied",
      properties: { sessionID: "old-child" },
    },
  });
  await plugin["chat.message"]({ sessionID: "old-child" });

  expect(requests.map(requestMethod)).toEqual(["pane.report_agent"]);
  expect(requests.map(requestState)).toEqual(["working"]);
  expect(requests.map(requestSessionID)).toEqual(["new-root"]);
});

test("dispose drops queued statuses before socket dispatch", async () => {
  autoAcknowledge = false;
  const plugin = await loadPlugin();
  const first = plugin.event(sessionStatusEvent("root-session", { type: "busy" }));
  const second = plugin.event(sessionStatusEvent("root-session", { type: "busy" }));

  await plugin.dispose();
  await Promise.all([first, second]);

  expect(requests).toHaveLength(0);
  expect(clients).toHaveLength(0);
});

test("dispose destroys an in-flight socket and resolves its request", async () => {
  autoAcknowledge = false;
  const plugin = await loadPlugin();
  const dispatched = waitForNextRequest();
  const pending = plugin.event(sessionStatusEvent("root-session", { type: "busy" }));
  await dispatched;

  expect(clients).toHaveLength(1);
  expect(clients[0]?.destroyed).toBe(false);
  await plugin.dispose();
  await pending;

  expect(clients[0]?.destroyed).toBe(true);
  expect(requests).toHaveLength(1);
});

test("a cached module can create a fresh plugin after disposal", async () => {
  const factory = await loadPluginFactory();
  const first = await factory();
  await first.dispose();

  const second = await factory();
  await second.event(sessionStatusEvent("root-session", { type: "busy" }));

  expect(requests.map(requestState)).toEqual(["working"]);
  expect(requests.map(requestSessionID)).toEqual(["root-session"]);
});

test("reports child prompts without replacing the root session", async () => {
  const plugin = await loadPlugin();
  await plugin["chat.message"]({ sessionID: "root-session" });
  requests.length = 0;

  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        info: { id: "child-session", parentID: "root-session" },
      },
    },
  });

  for (const type of ["permission.asked", "question.asked"]) {
    await plugin.event({ event: { type, properties: { sessionID: "child-session" } } });
  }
  for (const type of ["permission.replied", "question.replied", "question.rejected"]) {
    await plugin.event({ event: { type, properties: { sessionID: "child-session" } } });
  }

  expect(requests.map(requestState)).toEqual([
    "blocked",
    "blocked",
    "working",
    "working",
    "working",
  ]);
  expect(requests.map(requestSessionID)).toEqual([
    "root-session",
    "root-session",
    "root-session",
    "root-session",
    "root-session",
  ]);
});

test("retired nested child prompts cannot target the newly selected root", async () => {
  const plugin = await loadPlugin();
  for (const info of [
    { id: "child-session", parentID: "root-session" },
    { id: "nested-session", parentID: "child-session" },
  ]) {
    await plugin.event({ event: { type: "session.created", properties: { info } } });
  }
  selectedRootSessionID = "other-root";
  await plugin["chat.message"]({ sessionID: "other-root" });
  await plugin.event({
    event: { type: "permission.asked", properties: { sessionID: "nested-session" } },
  });
  await plugin.event({
    event: { type: "permission.replied", properties: { sessionID: "nested-session" } },
  });
  await plugin.event({
    event: { type: "session.idle", properties: { sessionID: "nested-session" } },
  });
  await plugin["chat.message"]({ sessionID: "nested-session" });

  expect(requests.map(requestState)).toEqual(["working"]);
  expect(requests.map(requestSessionID)).toEqual(["other-root"]);
});

test("nested child prompts report their selected root without opening nested panes", async () => {
  const plugin = await loadPlugin();
  await plugin["chat.message"]({ sessionID: "root-session" });
  for (const info of [
    { id: "child-session", parentID: "root-session" },
    { id: "nested-session", parentID: "child-session" },
  ]) {
    await plugin.event({ event: { type: "session.created", properties: { info } } });
  }
  requests.length = 0;
  for (const type of ["permission.asked", "permission.replied"]) {
    await plugin.event({ event: { type, properties: { sessionID: "nested-session" } } });
  }
  expect(requests.map(requestState)).toEqual(["blocked", "working"]);
  expect(requests.map(requestSessionID)).toEqual(["root-session", "root-session"]);
  expect(requests.map(requestMethod)).toEqual(["pane.report_agent", "pane.report_agent"]);
});

test("root prompts stay blocked until every request completes", async () => {
  const plugin = await loadPlugin();

  await plugin.event({
    event: {
      type: "permission.asked",
      properties: { id: "permission-1", sessionID: "root-session" },
    },
  });
  await plugin.event({
    event: {
      type: "question.asked",
      properties: { id: "question-1", sessionID: "root-session" },
    },
  });
  await plugin.event({
    event: {
      type: "permission.replied",
      properties: { requestID: "permission-1", sessionID: "root-session" },
    },
  });
  await plugin.event(sessionStatusEvent("root-session", { type: "busy" }));
  await plugin.event(sessionStatusEvent("root-session", { type: "idle" }));
  await plugin.event({
    event: {
      type: "question.rejected",
      properties: { requestID: "question-1", sessionID: "root-session" },
    },
  });

  expect(requests.map(requestState)).toEqual(["blocked", "blocked", "working"]);
  expect(requests.map(requestSessionID)).toEqual([
    "root-session",
    "root-session",
    "root-session",
  ]);
});

test("child prompt blocks survive root statuses until the child replies", async () => {
  const plugin = await loadPlugin();
  await plugin.event({
    event: {
      type: "session.created",
      properties: { sessionID: "root-session" },
    },
  });
  await plugin["chat.message"]({ sessionID: "root-session" });
  requests.length = 0;

  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "child-session",
        info: { id: "child-session", parentID: "root-session" },
      },
    },
  });
  await plugin.event({
    event: {
      type: "question.asked",
      properties: { id: "question-1", sessionID: "child-session" },
    },
  });
  await plugin.event(sessionStatusEvent("root-session", { type: "busy" }));
  await plugin.event(sessionStatusEvent("root-session", { type: "idle" }));
  await plugin.event({
    event: {
      type: "question.replied",
      properties: { requestID: "question-1", sessionID: "child-session" },
    },
  });

  expect(requests.map(requestState)).toEqual(["blocked", "working"]);
  expect(requests.map(requestSessionID)).toEqual(["root-session", "root-session"]);
});

function requestMethod(request: unknown): unknown {
  return isRecord(request) ? request.method : undefined;
}

function requestState(request: unknown): unknown {
  return requestParam(request, "state");
}

function requestSeq(request: unknown): unknown {
  return requestParam(request, "seq");
}

function requestSessionID(request: unknown): unknown {
  return requestParam(request, "agent_session_id");
}

function requestParam(request: unknown, name: string): unknown {
  if (!isRecord(request) || !isRecord(request.params)) {
    return undefined;
  }
  return request.params[name];
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
