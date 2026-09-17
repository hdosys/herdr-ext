// installed by herdr
// managed by herdr; reinstalling or updating the integration overwrites this file.
// HERDR_INTEGRATION_ID=opencode-tui
// HERDR_INTEGRATION_VERSION=26

import net from "node:net";

const SOURCE = "herdr:opencode";
const AGENT = "opencode";
const ROUTE_POLL_INTERVAL_MS = 100;
const SELECTION_RETRY_DELAYS_MS = [100, 400, 1_000];
const SUBAGENT_SESSION_ENV = "HERDR_OPENCODE_SUBAGENT_SESSION_ID";
const MAX_RESPONSE_CHARACTERS = 64 * 1024;

function requestOnce(method, params, signal) {
  const socketPath = process.env.HERDR_SOCKET_PATH;
  if (!socketPath || signal.aborted) {
    return Promise.resolve();
  }

  const socketEndpoint =
    process.platform === "win32" ? `\\\\.\\pipe\\${socketPath}` : socketPath;
  const request = {
    id: `${SOURCE}:tui:${Date.now()}:${Math.floor(Math.random() * 1_000_000)
      .toString()
      .padStart(6, "0")}`,
    method,
    params,
  };

  return new Promise((resolve) => {
    let client;
    let buffer = "";
    let finished = false;
    const finish = (response) => {
      if (finished) return;
      finished = true;
      clearTimeout(timeout);
      signal.removeEventListener("abort", abort);
      client?.destroy();
      resolve(response);
    };
    const abort = () => finish();
    const timeout = setTimeout(abort, 500);
    signal.addEventListener("abort", abort, { once: true });
    client = net.createConnection(socketEndpoint, () => {
      if (signal.aborted) return abort();
      client.write(`${JSON.stringify(request)}\n`);
    });
    client.on("data", (chunk) => {
      buffer += chunk.toString();
      if (buffer.length > MAX_RESPONSE_CHARACTERS) return abort();
      let newline;
      while ((newline = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, newline);
        buffer = buffer.slice(newline + 1);
        try {
          const response = JSON.parse(line);
          if (response?.id === request.id) return finish(response);
        } catch {
          // Only a complete, correlated response acknowledges this request.
        }
      }
    });
    client.on("error", abort);
    client.on("end", abort);
    client.on("close", abort);
  });
}

export default {
  id: "herdr.opencode.session-selection",
  tui: async (api) => {
    if (
      process.env.HERDR_ENV !== "1" ||
      !process.env.HERDR_SOCKET_PATH ||
      !process.env.HERDR_PANE_ID
    ) {
      return;
    }

    let selectedSessionID;
    let confirmedSessionID;
    let reportedChildState;
    let reportSeq = Date.now() * 1000;
    let retryIndex = 0;
    let nextReportAt = 0;
    let reportPending = false;
    const controller = new AbortController();
    const syncSelectedSession = async () => {
      if (controller.signal.aborted) return;
      const route = api.route.current;
      const sessionID = route?.name === "session" ? route.params?.sessionID : undefined;
      const session =
        typeof sessionID === "string" && sessionID
          ? api.state.session.get(sessionID)
          : undefined;
      const subagentSessionID = process.env[SUBAGENT_SESSION_ENV];
      const ownsSession = subagentSessionID
        ? sessionID === subagentSessionID
        : !session?.parentID;
      if (!session || !ownsSession) {
        selectedSessionID = undefined;
        confirmedSessionID = undefined;
        reportedChildState = undefined;
        retryIndex = 0;
        nextReportAt = 0;
        return;
      }
      if (sessionID !== selectedSessionID) {
        selectedSessionID = sessionID;
        confirmedSessionID = undefined;
        reportedChildState = undefined;
        retryIndex = 0;
        nextReportAt = 0;
      }
      if (reportPending) return;
      if (confirmedSessionID === sessionID) {
        if (!subagentSessionID || !api.state.ready) return;
        // Attach loads the TUI plugin, not a server lifecycle plugin. Reuse its
        // synchronized snapshot after selection is accepted, including activity
        // that began before this pane attached. The existing route check owns it.
        const status = api.state.session.status(sessionID)?.type;
        const blocked = api.state.session.permission(sessionID).length > 0 ||
          api.state.session.question(sessionID).length > 0;
        const state = blocked ? "blocked" : status === "busy" || status === "retry"
          ? "working" : status === "idle" || status === undefined ? "idle" : "unknown";
        if (state === reportedChildState) return;
        reportedChildState = state;
        reportPending = true;
        try {
          await requestOnce("pane.report_agent", {
            pane_id: process.env.HERDR_PANE_ID,
            source: SOURCE,
            agent: AGENT,
            agent_session_id: sessionID,
            state,
            seq: ++reportSeq,
          }, controller.signal);
        } finally {
          reportPending = false;
        }
        return;
      }
      if (Date.now() < nextReportAt) {
        return;
      }

      const reportingSessionID = sessionID;
      reportPending = true;
      let confirmed = false;
      try {
        const paneID = process.env.HERDR_PANE_ID;
        const response = await requestOnce("pane.report_agent_session", {
          pane_id: paneID,
          source: SOURCE,
          agent: AGENT,
          agent_session_id: reportingSessionID,
          session_start_source: "select",
        }, controller.signal);
        if (response?.result?.type === "ok") {
          // A report can be received but rejected or pending process detection.
          // Read back the existing authority instead of counting delivery attempts.
          const observed = await requestOnce("pane.get", { pane_id: paneID }, controller.signal);
          const pane = observed?.result?.type === "pane_info" ? observed.result.pane : undefined;
          const session = pane?.agent_session;
          confirmed = pane?.pane_id === paneID && session?.source === SOURCE &&
            session?.agent === AGENT && session?.kind === "id" &&
            session?.value === reportingSessionID;
        }
      } catch {
        // Best-effort reporting retries below while the selected route remains active.
      } finally {
        reportPending = false;
      }
      if (controller.signal.aborted) return;
      if (selectedSessionID !== reportingSessionID ||
          api.route.current?.params?.sessionID !== reportingSessionID) {
        retryIndex = 0;
        nextReportAt = 0;
        return;
      }
      if (confirmed) {
        confirmedSessionID = reportingSessionID;
        retryIndex = 0;
        nextReportAt = Number.POSITIVE_INFINITY;
        await syncSelectedSession();
        return;
      }
      const retryDelay = SELECTION_RETRY_DELAYS_MS[retryIndex];
      retryIndex += 1;
      nextReportAt = retryDelay === undefined ? Number.POSITIVE_INFINITY : Date.now() + retryDelay;
    };

    const stopStatus = api.event.on("session.status", (event) => {
      if (event.properties.sessionID === selectedSessionID &&
          retryIndex > SELECTION_RETRY_DELAYS_MS.length) {
        retryIndex = 0;
        nextReportAt = 0;
        void syncSelectedSession();
      }
    });
    const stopConnected = api.event.on("server.connected", () => {
      confirmedSessionID = undefined;
      reportedChildState = undefined;
      retryIndex = 0;
      nextReportAt = 0;
      void syncSelectedSession();
    });
    const routePoll = setInterval(() => void syncSelectedSession(), ROUTE_POLL_INTERVAL_MS);
    api.lifecycle.onDispose(() => {
      controller.abort();
      clearInterval(routePoll);
      stopStatus();
      stopConnected();
    });
    await syncSelectedSession();
  },
};
