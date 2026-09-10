// ZJ_AGENT_STATE_OPENCODE_PLUGIN=v1
//
// zj-agent-state bridge plugin for opencode, adapted from zj-radar's own
// vendored bridge (crates/cli/src/setup/opencode_plugin.js) — same event
// handling and subagent filtering (both load-bearing), pointed at
// hooks/opencode-status.py instead of `zj-radar notify opencode`.
//
// ASYNC SPAWN ONLY — the plugin runs in opencode's process; a wedged
// classifier must not freeze the TUI event loop. Spawns are never
// concurrent: status edges go out FIFO, tool-activity `running` refreshes
// coalesce to the latest unsent one. Each child has a hard ~10s kill timer.
//
// Event shapes are opencode ≥ 1.18 (`packages/schema/src/v1/*.ts`): every bus
// event carries a top-level `sessionID`; `session.*` also carry `info`
// (Session.Info, whose own key is `id`), `message.part.updated` carries `part`.

const CLASSIFIER = `${process.env.HOME}/Code/zj-agent-state/hooks/opencode-status.py`;

let CWD = "";

const childSessions = new Set();
const classifiedSessions = new Set();
let sdk = null;

async function classify(sessionID) {
  if (typeof sessionID !== "string" || classifiedSessions.has(sessionID)) return;
  classifiedSessions.add(sessionID);
  try {
    const { data } = await sdk.session.get({ sessionID });
    if (data && data.parentID) childSessions.add(sessionID);
  } catch {}
}

const messageRoles = new Map();
let lastAssistantText = "";
let errorLatched = false;

let pendingRunning = null;
const queue = [];
let processing = false;

function enqueue(status, payload) {
  const droppable = status === "running" && payload.event !== "chat.message";
  if (droppable) {
    pendingRunning = payload;
  } else {
    if (pendingRunning !== null) {
      queue.push({ status: "running", payload: pendingRunning });
      pendingRunning = null;
    }
    queue.push({ status, payload });
  }
  errorLatched = status === "error";
  processQueue();
}

async function processQueue() {
  if (processing) return;
  processing = true;
  while (queue.length > 0 || pendingRunning !== null) {
    let item = queue.shift();
    if (item === undefined) {
      item = { status: "running", payload: pendingRunning };
      pendingRunning = null;
    }
    try {
      await notify(item.status, item.payload);
    } catch {}
  }
  processing = false;
}

function notify(status, payload) {
  if (!process.env.ZELLIJ) return Promise.resolve();
  if (!Bun.which("python3")) return Promise.resolve();

  const data = JSON.stringify({ ...payload, cwd: CWD });
  let child;
  try {
    child = Bun.spawn(["python3", CLASSIFIER, "--status", status], {
      stdin: "pipe",
      stdout: "ignore",
      stderr: "ignore",
    });
  } catch {
    return Promise.resolve();
  }
  try {
    child.stdin.write(data);
    child.stdin.end();
  } catch {}
  const timer = setTimeout(() => {
    try { child.kill(); } catch {}
  }, 10_000);
  return child.exited
    .then(() => clearTimeout(timer))
    .catch(() => clearTimeout(timer));
}

function promptText(parts) {
  if (!Array.isArray(parts)) return "";
  return parts
    .filter((p) => p && p.type === "text" && typeof p.text === "string")
    .map((p) => p.text)
    .join("\n")
    .trim();
}

function errorMessage(error) {
  if (!error) return "";
  if (error.data && typeof error.data.message === "string" && error.data.message) {
    return error.data.message;
  }
  return typeof error.name === "string" ? error.name : "";
}

function permissionMessage(props) {
  const name = typeof props.permission === "string" && props.permission ? props.permission : "permission";
  const patterns = Array.isArray(props.patterns) ? props.patterns.join(", ") : "";
  return patterns ? `${name}: ${patterns}` : name;
}

function questionMessage(props) {
  const first = Array.isArray(props.questions) ? props.questions[0] : null;
  return first && typeof first.question === "string" ? first.question : "question";
}

function eventSession(props) {
  return props.sessionID || (props.info && props.info.id) || (props.part && props.part.sessionID) || null;
}

function isChild(sessionID) {
  return typeof sessionID === "string" && childSessions.has(sessionID);
}

function endTurn() {
  lastAssistantText = "";
  messageRoles.clear();
}

export const ZjAgentStatePlugin = async ({ directory, client }) => {
  CWD = typeof directory === "string" ? directory : "";
  sdk = client;
  return {
    "chat.message": async (input, output) => {
      const sessionID = input && input.sessionID;
      await classify(sessionID);
      if (!output || isChild(sessionID)) return;
      lastAssistantText = "";
      if (output.message && output.message.id) messageRoles.set(output.message.id, "user");
      enqueue("running", { event: "chat.message", prompt: promptText(output.parts) });
    },

    "tool.execute.before": async (input, output) => {
      if (isChild(input && input.sessionID)) return;
      enqueue("running", { event: "tool.execute", tool: input.tool, tool_input: output && output.args });
    },
    "tool.execute.after": async (input) => {
      if (isChild(input && input.sessionID)) return;
      enqueue("running", { event: "tool.execute", tool: input.tool, tool_input: input.args });
    },

    event: async ({ event }) => {
      const type = event && event.type;
      const props = (event && event.properties) || {};
      const sessionID = eventSession(props);

      switch (type) {
        case "permission.asked":
          enqueue("pending", { event: "permission.ask", message: permissionMessage(props) });
          return;
        case "question.asked":
          enqueue("pending", { event: "question.ask", message: questionMessage(props) });
          return;
        case "permission.replied":
        case "question.replied":
        case "question.rejected":
          enqueue("running", { event: "needs_you.replied" });
          return;

        case "session.created":
        case "session.updated":
          if (!sessionID) break;
          classifiedSessions.add(sessionID);
          if (props.info && props.info.parentID) {
            childSessions.add(sessionID);
            return;
          }
          break;
        case "session.deleted":
          classifiedSessions.delete(sessionID);
          if (childSessions.delete(sessionID)) return;
          break;
      }
      if (isChild(sessionID)) return;

      switch (type) {
        case "message.updated":
          if (props.info && props.info.id && props.info.role) messageRoles.set(props.info.id, props.info.role);
          break;
        case "message.part.updated": {
          const part = props.part;
          if (!part || part.type !== "text" || typeof part.text !== "string") break;
          if (messageRoles.get(part.messageID) === "assistant") lastAssistantText = part.text;
          break;
        }
        case "session.idle":
          if (!errorLatched) enqueue("done", { event: "session.idle", message: lastAssistantText });
          endTurn();
          break;
        case "session.error":
          if (props.error && props.error.name === "MessageAbortedError") break;
          enqueue("error", { event: "session.error", message: errorMessage(props.error) });
          endTurn();
          break;
        case "session.created":
        case "session.deleted":
          enqueue("idle", { event: "session.lifecycle" });
          endTurn();
          break;
      }
    },
  };
};

export default ZjAgentStatePlugin;
