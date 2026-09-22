/**
 * MoonlightCode Core — the one backend connection the other extensions share.
 *
 * It contributes no UI. It exists so that the feature extensions can ship and be
 * installed separately without each one opening its own connection, running its own
 * poll loop, and forming its own opinion about what the fleet is doing.
 */
import * as vscode from 'vscode';
import type { MoonlightApi } from './api';
export declare function activate(context: vscode.ExtensionContext): MoonlightApi;
export declare function deactivate(): void;
