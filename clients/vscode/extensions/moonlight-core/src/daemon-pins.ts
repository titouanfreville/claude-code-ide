/**
 * The daemon build this extension is allowed to download, and its hashes.
 *
 * GENERATED AT RELEASE TIME — `scripts/pin-daemon.js` overwrites this file in CI
 * after the daemon matrix has built, so the hashes are computed from the exact
 * artifacts the same release publishes. Nothing here is edited by hand.
 *
 * It is `undefined` in the repo on purpose. A locally packaged `.vsix` then simply
 * never downloads, and falls back to `PATH` and the `moonlight.daemon.path` setting
 * — the behaviour developers already have. Shipping a real-looking placeholder would
 * instead make a dev build reach for a release that may not exist.
 */
export interface DaemonPins {
  /** Version string in the asset file names (e.g. `0.1.1`, `nightly-abc1234`). */
  readonly version: string;
  /**
   * Git tag holding the assets.
   *
   * Explicit rather than derived from {@link version}, because the nightly release
   * keeps a single rolling `nightly` tag whose assets are named for the commit — so
   * `v${version}` is a tag that exists only for versioned releases.
   */
  readonly tag: string;
  /** Rust target triple -> SHA-256 of the gzipped asset, lowercase hex. */
  readonly assets: Readonly<Record<string, string>>;
}

export const DAEMON_PINS: DaemonPins | undefined = undefined;
