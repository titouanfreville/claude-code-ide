/**
 * The findings a reviewer hands over, and how they are read.
 *
 * Deliberately producer-neutral. bmad, Claude Code's own review, Partoo's reviewer and
 * the `deep-review` skill all emit findings, and they agree on almost nothing beyond
 * "here is a thing I think is wrong". So only `id` and `summary` are required, and
 * every other field renders when present. A schema that demanded severity, or a route,
 * or the lanes that raised it, would be a schema only one producer could satisfy.
 *
 * No `vscode` import: this is the part that has to be right, and it is tested directly.
 */

/** What the operator decided about one finding — the plan panel's vocabulary. */
export type FindingVerdict = 'approve' | 'open-question' | 'refine' | 'no-go';

/** Every verdict, in the order the panel offers them. */
export const FINDING_VERDICTS: readonly FindingVerdict[] = [
  'approve',
  'refine',
  'no-go',
  'open-question',
];

/**
 * One reviewer's claim about the code.
 *
 * `extra` holds whatever the producer sent that this schema does not name. It is kept
 * and shown rather than dropped: a reviewer that silently discards half a finding is
 * worse than one that refuses to load it, because nobody can tell it happened.
 */
export interface Finding {
  /** Stable within one review — the operator's verdict is recorded against it. */
  readonly id: string;
  /** One line. The only thing guaranteed to be renderable. */
  readonly summary: string;
  /** Markdown. The argument for the finding. */
  readonly detail?: string;
  /** Markdown. What the producer proposes doing about it. */
  readonly fix?: string;
  readonly severity?: string;
  readonly file?: string;
  readonly line?: number;
  /** Which reviewer, lane or rule raised it. */
  readonly source?: string;
  /** Fields this schema does not name, preserved verbatim. */
  readonly extra?: Readonly<Record<string, unknown>>;
}

/** A complete review, as handed to the panel. */
export interface FindingsReview {
  readonly title: string;
  readonly findings: readonly Finding[];
}

/** Raised when a payload cannot be read. Carries a message meant for a person. */
export class FindingsParseError extends Error {}

const KNOWN = new Set(['id', 'summary', 'detail', 'fix', 'severity', 'file', 'line', 'source']);

function asString(value: unknown): string | undefined {
  return typeof value === 'string' && value.trim() !== '' ? value : undefined;
}

/**
 * Severity order, most serious first. Anything unrecognised — or absent — sorts last
 * rather than being guessed at: a producer with its own scale should not have its
 * findings silently promoted because one of its words happened to match ours.
 */
const SEVERITY_RANK = new Map<string, number>([
  ['critical', 0],
  ['blocker', 0],
  ['high', 1],
  ['major', 1],
  ['medium', 2],
  ['moderate', 2],
  ['low', 3],
  ['minor', 3],
  ['info', 4],
  ['nit', 4],
]);

/** Where a finding sorts. Unknown severities keep their input order, after the known. */
export function severityRank(severity: string | undefined): number {
  if (severity === undefined) {
    return 99;
  }
  return SEVERITY_RANK.get(severity.trim().toLowerCase()) ?? 98;
}

/**
 * Read a review payload.
 *
 * Throws rather than returning a partial review. The operator is about to make a
 * decision per finding and hand it back to a blocked agent — a list quietly missing
 * entries produces a verdict about a review that was never shown.
 */
export function parseReview(input: unknown): FindingsReview {
  let value = input;
  if (typeof value === 'string') {
    try {
      value = JSON.parse(value);
    } catch (err) {
      throw new FindingsParseError(
        `not valid JSON: ${err instanceof Error ? err.message : String(err)}`
      );
    }
  }
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw new FindingsParseError('expected an object with a `findings` array');
  }
  const root = value as Record<string, unknown>;
  const rawFindings = root.findings;
  if (!Array.isArray(rawFindings)) {
    throw new FindingsParseError('`findings` must be an array');
  }
  if (rawFindings.length === 0) {
    // A blocking gate over nothing would hold an agent on an empty panel.
    throw new FindingsParseError('`findings` is empty — there is nothing to review');
  }

  const seen = new Set<string>();
  const findings = rawFindings.map((raw, index) => {
    if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) {
      throw new FindingsParseError(`finding ${index + 1} is not an object`);
    }
    const f = raw as Record<string, unknown>;
    const summary = asString(f.summary);
    if (summary === undefined) {
      throw new FindingsParseError(`finding ${index + 1} has no \`summary\``);
    }
    // An id is what a verdict is recorded against, so a generated one is fine but a
    // duplicate is not: two findings sharing an id means one operator decision is
    // silently applied to both.
    const id = asString(f.id) ?? `finding-${index + 1}`;
    if (seen.has(id)) {
      throw new FindingsParseError(`duplicate finding id \`${id}\``);
    }
    seen.add(id);

    const extra: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(f)) {
      if (!KNOWN.has(k)) {
        extra[k] = v;
      }
    }
    const line = typeof f.line === 'number' && Number.isFinite(f.line) ? f.line : undefined;
    return {
      id,
      summary,
      detail: asString(f.detail),
      fix: asString(f.fix),
      severity: asString(f.severity),
      file: asString(f.file),
      line,
      source: asString(f.source),
      ...(Object.keys(extra).length > 0 ? { extra } : {}),
    } satisfies Finding;
  });

  return {
    title: asString(root.title) ?? 'Review findings',
    // Stable sort: equal severities keep the producer's order, which is usually
    // meaningful (it grouped them by file, or by the lane that raised them).
    findings: findings
      .map((f, i) => ({ f, i }))
      .sort((a, b) => severityRank(a.f.severity) - severityRank(b.f.severity) || a.i - b.i)
      .map(({ f }) => f),
  };
}
