"use strict";
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __setModuleDefault = (this && this.__setModuleDefault) || (Object.create ? (function(o, v) {
    Object.defineProperty(o, "default", { enumerable: true, value: v });
}) : function(o, v) {
    o["default"] = v;
});
var __importStar = (this && this.__importStar) || (function () {
    var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function (o) {
            var ar = [];
            for (var k in o) if (Object.prototype.hasOwnProperty.call(o, k)) ar[ar.length] = k;
            return ar;
        };
        return ownKeys(o);
    };
    return function (mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        __setModuleDefault(result, mod);
        return result;
    };
})();
Object.defineProperty(exports, "__esModule", { value: true });
exports.SessionTerminals = void 0;
const vscode = __importStar(require("vscode"));
const controlApi = __importStar(require("moonlight-control-client"));
/**
 * Terminals this extension family started, by session id.
 *
 * Only terminals we created are here. We could try to *find* the terminal hosting a
 * session we didn't start — `window.terminals` exposes every terminal and
 * `Terminal.processId` gives its pid — but matching a pid to a session means walking
 * the process tree for a `claude` process carrying that id, and that fails for a
 * session in an external terminal, a multiplexer, or another window. A wrong match
 * would type a review into an unrelated shell, so this only claims what it knows.
 */
class SessionTerminals {
    owned = new Map();
    constructor(context) {
        // A closed terminal can't receive anything. Dropping it here is what makes
        // delivery fall through to the honest path rather than writing into a corpse.
        context.subscriptions.push(vscode.window.onDidCloseTerminal((closed) => {
            for (const [id, term] of [...this.owned]) {
                if (term === closed) {
                    this.owned.delete(id);
                }
            }
        }));
    }
    start(sessionId, cwd) {
        const terminal = vscode.window.createTerminal({
            name: `Claude (${sessionId.slice(0, 8)})`,
            cwd,
        });
        this.owned.set(sessionId, terminal);
        // The command is typed once the MCP endpoint is bound, which needs a round-trip.
        // The terminal is created and shown first so the operator sees the session coming
        // up rather than an empty pause.
        void this.launch(terminal, sessionId);
        terminal.show();
        return terminal;
    }
    /**
     * Type the launch command, with this session's `moonlight` MCP server wired in.
     *
     * The endpoint has to be bound *before* launching, because its URL goes into the
     * launch arguments — and it cannot be bound in advance, since it belongs to a
     * session id we mint here. A session launched without it is gated by the hooks but
     * has no `moonlight` verbs: no `present_plan`, no `request_phase`, no `phase_status`,
     * no `report_blocked`. From the operator's side that looks like an agent that cannot
     * get out of `Plan` and will not say why.
     *
     * Best-effort, like the desktop app: a backend too old to bind one (or built without
     * the MCP transport) still gets a session, with a warning that says what is missing.
     * Refusing to start would be a worse trade.
     */
    async launch(terminal, sessionId) {
        let flags = '';
        // The id is ours (minted below), but it is interpolated into a shell line, so it is
        // checked rather than trusted — a session id that is not the shape we mint means
        // something upstream changed, and the right move is to not build a command from it.
        if (!controlApi.isSafeSessionId(sessionId)) {
            void vscode.window.showErrorMessage(`MoonlightCode: refusing to launch — unexpected session id shape (${sessionId}).`);
            return;
        }
        try {
            const { url } = await controlApi.mcpEndpoint(sessionId);
            const flag = controlApi.mcpConfigFlag(url);
            if (flag === undefined) {
                // A non-loopback or non-http endpoint is not something we put on a command
                // line. Launch without the verbs and say so, rather than running it.
                void vscode.window.showWarningMessage(`MoonlightCode: refusing an unexpected MCP endpoint (${url}). The session starts without the moonlight verbs.`);
            }
            else {
                flags = flag;
            }
        }
        catch (err) {
            const message = err instanceof Error ? err.message : String(err);
            void vscode.window.showWarningMessage(`MoonlightCode: no MCP endpoint for this session (${message}). It starts without the moonlight verbs — it cannot propose a plan or request a phase.`);
        }
        // `--session-id` pins the id, the same flag the desktop app uses, so the session
        // we start is the session we can adopt, govern and deliver reviews to.
        terminal.sendText(`claude --session-id ${sessionId}${flags}`, true);
    }
    get(sessionId) {
        return this.owned.get(sessionId);
    }
    has(sessionId) {
        return this.owned.has(sessionId);
    }
    adopt(sessionId, terminal) {
        this.owned.set(sessionId, terminal);
    }
}
exports.SessionTerminals = SessionTerminals;
//# sourceMappingURL=terminals.js.map