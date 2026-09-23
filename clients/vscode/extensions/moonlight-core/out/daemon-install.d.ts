import { type DaemonPins } from './daemon-pins';
/**
 * The Rust target triple for a Node platform/arch pair, or `undefined` where the
 * release publishes nothing.
 *
 * Kept total and pure so the mapping can be tested without a network or a filesystem
 * — it is the piece most likely to drift when the build matrix gains a target.
 */
export declare function targetTriple(platform?: string, arch?: string): string | undefined;
/** Asset file name for a target, matching what `build.yml` uploads. */
export declare function assetName(version: string, target: string): string;
/** Full download URL for a target's asset under a given release tag. */
export declare function assetUrl(tag: string, version: string, target: string): string;
/** Why a daemon was not installed, for the caller to surface verbatim. */
export type InstallResult = {
    kind: 'installed';
    binary: string;
} | {
    kind: 'cached';
    binary: string;
} | {
    kind: 'unpinned';
} | {
    kind: 'unsupported-platform';
    platform: string;
    arch: string;
} | {
    kind: 'failed';
    error: string;
};
/**
 * Decide what a download attempt should do, given what is already on disk.
 *
 * Split out from the I/O so the precedence — pinned, supported, already cached — can
 * be tested directly; the network path below is then only the part that genuinely
 * needs a network.
 */
export declare function planInstall(pins: DaemonPins | undefined, target: string | undefined, cachedExists: boolean): {
    action: 'download';
    tag: string;
    version: string;
    target: string;
    sha256: string;
} | InstallResult;
/** Lowercase hex SHA-256, the form the pins are generated in. */
export declare function sha256(data: Buffer): string;
/**
 * Ensure a verified daemon exists in `storageDir`, downloading it if needed.
 *
 * Returns the binary's path on success. The caller passes it to `ensureDaemon` as
 * `options.path`, which already wins over `PATH` — so an operator who set
 * `moonlight.daemon.path` themselves is never overridden by a download.
 */
export declare function ensureDownloadedDaemon(storageDir: string, binaryName: string, pins?: DaemonPins | undefined): Promise<InstallResult>;
