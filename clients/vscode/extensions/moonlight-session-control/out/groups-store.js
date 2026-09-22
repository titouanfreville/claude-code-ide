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
exports.GroupStore = void 0;
/**
 * Where the operator's custom session groups live, and the commands that edit them.
 *
 * `workspaceState`, matching `moonlight-core`'s session pins — a group is a way of
 * arranging the work in front of you, and the sessions in it are usually the ones this
 * window is about. Moving to `globalState` later is a one-line change here and nothing
 * elsewhere, which is why the storage sits behind this type rather than being read at
 * the call sites.
 */
const vscode = __importStar(require("vscode"));
const GROUPS_KEY = 'moonlight.sessionGroups';
/** Reads the grouping mode from settings and the groups from workspace storage. */
class GroupStore {
    memento;
    changed = new vscode.EventEmitter();
    /** Fires when a group is created, edited or removed, so the views can refresh. */
    onDidChange = this.changed.event;
    constructor(memento) {
        this.memento = memento;
    }
    /**
     * Read per call rather than cached, so changing the setting takes effect on the next
     * refresh instead of needing a window reload — the same reasoning as the daemon
     * options in core.
     */
    mode() {
        const value = vscode.workspace
            .getConfiguration('moonlight')
            .get('sessions.groupBy');
        return value === 'custom' || value === 'none' ? value : 'project';
    }
    custom() {
        return this.memento.get(GROUPS_KEY) ?? [];
    }
    async write(groups) {
        await this.memento.update(GROUPS_KEY, groups);
        this.changed.fire();
    }
    async create(name) {
        // Time-based rather than a name slug: two groups may share a name, and a key that
        // changed when a group was renamed would drop the tree's expansion state. The
        // random suffix is what makes it an id — `Date.now()` alone collides for two groups
        // made in the same millisecond, after which rename and delete hit both.
        const id = `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
        const group = { id, name, sessionIds: [] };
        await this.write([...this.custom(), group]);
        return group;
    }
    async rename(id, name) {
        await this.write(this.custom().map((g) => (g.id === id ? { ...g, name } : g)));
    }
    async remove(id) {
        await this.write(this.custom().filter((g) => g.id !== id));
    }
    /**
     * Put a session in a group, taking it out of any other.
     *
     * One group per session, deliberately: a session in two groups renders twice, and a
     * status badge appearing in two places reads as two sessions in trouble.
     */
    async assign(sessionId, groupId) {
        await this.write(this.custom().map((g) => ({
            ...g,
            sessionIds: g.id === groupId
                ? [...g.sessionIds.filter((id) => id !== sessionId), sessionId]
                : g.sessionIds.filter((id) => id !== sessionId),
        })));
    }
    async unassign(sessionId) {
        await this.write(this.custom().map((g) => ({
            ...g,
            sessionIds: g.sessionIds.filter((id) => id !== sessionId),
        })));
    }
    dispose() {
        this.changed.dispose();
    }
}
exports.GroupStore = GroupStore;
//# sourceMappingURL=groups-store.js.map