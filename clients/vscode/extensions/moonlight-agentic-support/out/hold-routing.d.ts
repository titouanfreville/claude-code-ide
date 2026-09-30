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
export declare function isPlanHold(hold: RoutableHold): boolean;
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
export declare function shouldActUnprompted(ownsSession: boolean | undefined): boolean;
