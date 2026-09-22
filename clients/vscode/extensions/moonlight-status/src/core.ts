/**
 * Reaching MoonlightCode Core from a dependent extension.
 *
 * `extensionDependencies` makes VSCode install and activate core first, so by the
 * time our `activate` runs its exports exist. The checks here cover what that
 * guarantee does not: a stale core from a partial update, or a version bump.
 *
 * The API type is imported `import type`, and the id is a local constant, so nothing
 * here pulls core's runtime module into this extension's bundle.
 */
import * as vscode from 'vscode';
import type { MoonlightApi } from 'moonlight-core';

const CORE_EXTENSION_ID = 'titouanfreville.moonlight-core';

/** The oldest core this extension can work against. */
const MINIMUM_CORE_VERSION = 1;

export async function requireCore(): Promise<MoonlightApi | undefined> {
  const ext = vscode.extensions.getExtension<MoonlightApi>(CORE_EXTENSION_ID);
  if (!ext) {
    void vscode.window.showErrorMessage(
      'MoonlightCode Core is not installed. Install the MoonlightCode extension pack.'
    );
    return undefined;
  }
  const api = ext.isActive ? ext.exports : await ext.activate();
  // A *minimum*, not an equality. Core's version rises when it adds members — the
  // held-approval surface made it 2 — and an equality check turned every such
  // addition into a silent shutdown of the extensions that did not even need it.
  // Members added after MINIMUM_CORE_VERSION are optional on the interface, so this
  // extension feature-detects them instead of demanding a version.
  if (typeof api?.version !== 'number' || api.version < MINIMUM_CORE_VERSION) {
    void vscode.window.showErrorMessage(
      `MoonlightCode Core exposes API version ${api?.version ?? 'unknown'}, which this extension does not understand. Update the MoonlightCode extensions together.`
    );
    return undefined;
  }
  return api;
}
