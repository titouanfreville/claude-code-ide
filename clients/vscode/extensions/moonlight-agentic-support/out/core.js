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
exports.requireCore = requireCore;
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
const vscode = __importStar(require("vscode"));
const CORE_EXTENSION_ID = 'titouanfreville.moonlight-core';
/** The oldest core this extension can work against. */
const MINIMUM_CORE_VERSION = 1;
async function requireCore() {
    const ext = vscode.extensions.getExtension(CORE_EXTENSION_ID);
    if (!ext) {
        void vscode.window.showErrorMessage('MoonlightCode Core is not installed. Install the MoonlightCode extension pack.');
        return undefined;
    }
    const api = ext.isActive ? ext.exports : await ext.activate();
    // A *minimum*, not an equality. Core's version rises when it adds members — the
    // held-approval surface made it 2 — and an equality check turned every such
    // addition into a silent shutdown of the extensions that did not even need it.
    // Members added after MINIMUM_CORE_VERSION are optional on the interface, so this
    // extension feature-detects them instead of demanding a version.
    if (typeof api?.version !== 'number' || api.version < MINIMUM_CORE_VERSION) {
        void vscode.window.showErrorMessage(`MoonlightCode Core exposes API version ${api?.version ?? 'unknown'}, which this extension does not understand. Update the MoonlightCode extensions together.`);
        return undefined;
    }
    return api;
}
//# sourceMappingURL=core.js.map