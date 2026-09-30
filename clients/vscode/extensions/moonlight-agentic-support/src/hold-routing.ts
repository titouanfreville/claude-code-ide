/**
 * Which surface answers a hold, and in which window.
 *
 * Both decisions used to live inline in `announce` and `answerSomeHold`, spelled
 * slightly differently in each — which is how one of them kept the predicate the other
 * had just been fixed to drop. They are pure, they are the part that has to be right,
 * and nothing here imports `vscode`, so they are tested directly.
 */
import { PLAN_HOLD } from 'moonlight-control-client';

/** Just enough of a held approval to route it. */
export interface RoutableHold {
  readonly what: string;
}

/**
 * Whether this hold is the plan proposal, and so belongs in the plan panel.
 *
 * The gate's own marker, not "does this hold carry a plan text". The held-set fold
 * caches a session's plan so a later `ApprovalRequested` can be matched to it, and that
 * cache outlives the hold — so a `plan !== undefined` test was true for every hold a
 * session raised after its first plan, and a `Bash` approval opened the plan review
 * panel instead of asking about the command.
 */
export function isPlanHold(hold: RoutableHold): boolean {
  return hold.what === PLAN_HOLD;
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
export function shouldActUnprompted(ownsSession: boolean | undefined): boolean {
  return ownsSession !== false;
}
