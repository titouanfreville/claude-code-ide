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
exports.activate = activate;
exports.deactivate = deactivate;
/**
 * MoonlightCode Agentic Support — the plan gate and held-approval surface.
 *
 * A session proposing a plan, or attempting a danger-zone action, *holds* at the gate
 * waiting for a human answer. The desktop cockpit has surfaces for both; until this
 * extension existed an editor had neither, so from VSCode the only way to answer a
 * hold was to open a different application.
 *
 * Why that matters more than a missing feature usually does: the session is stopped
 * for as long as the hold is open. A bounded hold with no answer resolves to a
 * **deny**, by design — the server decides cleanly instead of failing open — and an
 * unbounded one (what the daemon uses when a client is expected) waits forever. Both
 * failure modes look to the operator like an agent that mysteriously stopped working.
 *
 * The state comes from core, which owns the single event-stream connection, so this
 * extension never has to reconcile its own view of what is held.
 */
const vscode = __importStar(require("vscode"));
const core_1 = require("./core");
const gate_1 = require("./gate");
const plan_view_1 = require("./plan-view");
async function activate(context) {
    const core = await (0, core_1.requireCore)();
    if (!core) {
        return;
    }
    context.subscriptions.push(vscode.commands.registerCommand('moonlight.gate.reviewPlan', () => {
        plan_view_1.PlanReviewPanel.show(core, undefined);
    }), vscode.commands.registerCommand('moonlight.gate.answerHold', () => answerSomeHold(core)));
    // An older core has no gate state to offer; the commands above still work as far as
    // they can, which is better than an extension that refuses to activate.
    if (!core.onDidChangeHeldApprovals) {
        return;
    }
    const announced = new Set();
    context.subscriptions.push(core.onDidChangeHeldApprovals(() => {
        const holds = core.heldApprovals?.() ?? [];
        const live = new Set(holds.map((hold) => hold.sessionId));
        // Forget the ones that were answered, so the same session holding again is
        // announced again rather than being mistaken for the notification still open.
        for (const sessionId of [...announced]) {
            if (!live.has(sessionId)) {
                announced.delete(sessionId);
            }
        }
        for (const hold of holds) {
            if (announced.has(hold.sessionId)) {
                continue;
            }
            announced.add(hold.sessionId);
            void announce(core, hold);
        }
    }));
}
/**
 * Tell the operator a session is blocked, and offer the answer in the same breath.
 *
 * A plan goes to the panel — a plan is read, not dispatched from a notification. A
 * danger-zone or MCP-authorize hold is a single question, so it is answered inline.
 */
async function announce(core, hold) {
    const short = hold.sessionId.slice(0, 8);
    const isPlan = hold.plan !== undefined || hold.what === 'approve plan';
    if (isPlan) {
        // Opened directly rather than offered behind a notification. The session is
        // *stopped* until this is answered, and a toast is the wrong shape for that: it
        // auto-dismisses, it stacks behind whatever else fired, and a plan is read rather
        // than dispatched from a one-line prompt — so the common outcome was a blocked
        // agent and a notification nobody saw. A danger-zone hold below is genuinely one
        // question and stays inline.
        plan_view_1.PlanReviewPanel.show(core, hold.sessionId);
        return;
    }
    const tool = hold.mcpTool ? ` (${hold.mcpTool})` : '';
    const choice = await vscode.window.showWarningMessage(`MoonlightCode: session ${short} is held — ${hold.what}${tool}`, { modal: false }, 'Allow', 'Refuse');
    if (choice === undefined) {
        // Dismissing is not an answer, and pretending it is would deny an action the
        // operator never ruled on. The hold stays; `moonlight.gate.answerHold` reopens it.
        return;
    }
    await applyActionVerdict(hold, choice === 'Allow');
}
/** Answer whichever hold is outstanding, on demand rather than on notification. */
async function answerSomeHold(core) {
    const holds = core.heldApprovals?.() ?? [];
    if (holds.length === 0) {
        void vscode.window.showInformationMessage('MoonlightCode: nothing is waiting on you.');
        return;
    }
    const picked = holds.length === 1
        ? holds[0]
        : await pickHold(holds);
    if (!picked) {
        return;
    }
    if (picked.plan !== undefined || picked.what === 'approve plan') {
        plan_view_1.PlanReviewPanel.show(core, picked.sessionId);
        return;
    }
    const choice = await vscode.window.showWarningMessage(`${picked.what}${picked.mcpTool ? ` (${picked.mcpTool})` : ''}`, { modal: true }, 'Allow', 'Refuse');
    if (choice === undefined) {
        return;
    }
    await applyActionVerdict(picked, choice === 'Allow');
}
async function pickHold(holds) {
    const items = holds.map((hold) => ({
        label: `${hold.sessionId.slice(0, 8)} — ${hold.what}`,
        description: `waiting ${Math.round((Date.now() - hold.sinceMs) / 1000)}s`,
        hold,
    }));
    const picked = await vscode.window.showQuickPick(items, {
        placeHolder: 'Which hold do you want to answer?',
    });
    return picked?.hold;
}
/**
 * Refusing asks for a reason first. The reason is returned to the agent as the hook's
 * deny message — it is the only thing the session learns from being refused, so a
 * silent "no" leaves it to guess what it did wrong and try something similar.
 */
async function applyActionVerdict(hold, allow) {
    let reason = 'Refused by the operator.';
    if (!allow) {
        const typed = await vscode.window.showInputBox({
            prompt: 'Why is this refused? The agent is told, and acts on it.',
            value: reason,
        });
        if (typed === undefined) {
            return; // Cancelled the refusal; the hold stays open.
        }
        reason = typed.trim().length > 0 ? typed.trim() : reason;
    }
    const outcome = await (0, gate_1.decideAction)(hold.sessionId, allow, reason);
    if (outcome.ok) {
        void vscode.window.showInformationMessage(`MoonlightCode: ${outcome.summary}`);
    }
    else {
        void vscode.window.showErrorMessage(`MoonlightCode: the verdict did not reach the session (${outcome.error}). It is still waiting.`);
    }
}
function deactivate() { }
//# sourceMappingURL=extension.js.map