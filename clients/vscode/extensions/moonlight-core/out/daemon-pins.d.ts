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
    /**
     * Rust target triple -> the two hashes that target's daemon must match.
     *
     * Two, because they are checked at different moments against different bytes.
     * `asset` is the gzipped download and is verified *before* decompressing, so a
     * substituted or corrupt asset is never handed to gunzip. `binary` is the
     * decompressed executable, and is what lets a cached daemon be re-verified on every
     * activation — the download hash cannot do that, because gzip is not reproducible
     * and the file on disk is no longer the bytes that were fetched.
     */
    readonly assets: Readonly<Record<string, {
        readonly asset: string;
        readonly binary: string;
    }>>;
}
export declare const DAEMON_PINS: DaemonPins | undefined;
