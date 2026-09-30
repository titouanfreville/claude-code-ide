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
const assert = __importStar(require("node:assert/strict"));
const node_test_1 = require("node:test");
const moonlight_control_client_1 = require("moonlight-control-client");
const hold_routing_1 = require("./hold-routing");
(0, node_test_1.test)('only the gate\u2019s plan marker routes to the plan panel', () => {
    assert.equal((0, hold_routing_1.isPlanHold)({ what: moonlight_control_client_1.PLAN_HOLD }), true);
    // The bug this replaced: a session that had ever proposed a plan had one cached
    // against it, so every later hold looked like a plan and a command approval opened
    // the plan review panel.
    assert.equal((0, hold_routing_1.isPlanHold)({ what: 'run `npx js-yaml`' }), false);
    assert.equal((0, hold_routing_1.isPlanHold)({ what: 'write outside the workspace' }), false);
});
(0, node_test_1.test)('a core too old to answer keeps the old behaviour', () => {
    // `undefined` is not `false`. Written as a function because a refactor turning
    // `=== false` into `!== true` would stop the plan panel opening anywhere at all
    // against an older core — the worse of the two failures.
    assert.equal((0, hold_routing_1.shouldActUnprompted)(undefined), true);
    assert.equal((0, hold_routing_1.shouldActUnprompted)(true), true);
    assert.equal((0, hold_routing_1.shouldActUnprompted)(false), false);
});
//# sourceMappingURL=hold-routing.test.js.map