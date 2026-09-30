"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.isPlanHold = isPlanHold;
exports.shouldActUnprompted = shouldActUnprompted;
/**
 * Which surface answers a hold, and in which window.
 *
 * Both decisions used to live inline in `announce` and `answerSomeHold`, spelled
 * slightly differently in each — which is how one of them kept the predicate the other
 * had just been fixed to drop. They are pure, they are the part that has to be right,
 * and nothing here imports `vscode`, so they are tested directly.
 */
const moonlight_control_client_1 = require("moonlight-control-client");
/**
 * Whether this hold is the plan proposal, and so belongs in the plan panel.
 *
 * The gate's own marker, not "does this hold carry a plan text". The held-set fold
 * caches a session's plan so a later `ApprovalRequested` can be matched to it, and that
 * cache outlives the hold — so a `plan !== undefined` test was true for every hold a
 * session raised after its first plan, and a `Bash` approval opened the plan review
 * panel instead of asking about the command.
 */
function isPlanHold(hold) {
    return hold.what === moonlight_control_client_1.PLAN_HOLD;
}
/**
 * Whether this window should open the surface itself, rather than offering it.
 *
 * Three states, and the third is the one worth spelling out: `undefined` means the core
 * is too old to say which window a session belongs to. That keeps the previous
 * behaviour — open — because a panel that opens in several windows is a nuisance, while
 * one that opens in none leaves an agent held with nobody told. Written as a function
 * so the tri-state is asserted rather than left to a `=== false` a refactor could turn
 * into `!== true`.
 */
function shouldActUnprompted(ownsSession) {
    return ownsSession !== false;
}
//# sourceMappingURL=hold-routing.js.map