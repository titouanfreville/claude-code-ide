"use strict";
var __importDefault = (this && this.__importDefault) || function (mod) {
    return (mod && mod.__esModule) ? mod : { "default": mod };
};
Object.defineProperty(exports, "__esModule", { value: true });
const strict_1 = __importDefault(require("node:assert/strict"));
const node_test_1 = require("node:test");
const events_1 = require("./events");
/**
 * A chunk boundary falls wherever the network put it. Treating a half-arrived frame as
 * complete would hand `JSON.parse` a truncated event — and the frame that gets split
 * is disproportionately likely to be a big one, which is to say a plan.
 */
(0, node_test_1.test)('a frame split across chunks is held until its terminator arrives', () => {
    const first = (0, events_1.readFrames)('data: {"SessionRemoved":{"ses');
    strict_1.default.deepEqual(first.frames, []);
    const second = (0, events_1.readFrames)(`${first.rest}sion":"s1"}}\n\n`);
    strict_1.default.equal(second.frames.length, 1);
    strict_1.default.equal(second.rest, '');
    strict_1.default.deepEqual((0, events_1.parseEngineEvent)(second.frames[0].data), {
        kind: 'SessionRemoved',
        session: 's1',
    });
});
(0, node_test_1.test)('keep-alive comments and blank lines carry no frame', () => {
    const { frames } = (0, events_1.readFrames)(':\n\n: keep-alive\n\n');
    strict_1.default.deepEqual(frames, []);
});
/** A lag is the server saying "your view is incomplete" — it must not read as data. */
(0, node_test_1.test)('a lagged frame keeps its event name', () => {
    const { frames } = (0, events_1.readFrames)('event: lagged\ndata: 12\n\n');
    strict_1.default.deepEqual(frames, [{ event: 'lagged', data: '12' }]);
});
(0, node_test_1.test)('a multi-line data payload is rejoined', () => {
    const { frames } = (0, events_1.readFrames)('data: {"a":\ndata: 1}\n\n');
    strict_1.default.equal(frames[0].data, '{"a":\n1}');
});
(0, node_test_1.test)('a plan proposal carries its markdown', () => {
    const event = (0, events_1.parseEngineEvent)('{"PlanProposed":{"session":"s1","plan":"# Step one"}}');
    strict_1.default.deepEqual(event, { kind: 'PlanProposed', session: 's1', plan: '# Step one' });
});
/** Plain plan and danger-zone holds have no tool; only an MCP-authorize hold does. */
(0, node_test_1.test)('an approval request reports its tool only when there is one', () => {
    strict_1.default.deepEqual((0, events_1.parseEngineEvent)('{"ApprovalRequested":{"session":"s1","what":"approve plan","authorize_tool":null}}'), { kind: 'ApprovalRequested', session: 's1', what: 'approve plan', authorizeTool: undefined });
    strict_1.default.deepEqual((0, events_1.parseEngineEvent)('{"ApprovalRequested":{"session":"s1","what":"run a tool","authorize_tool":"mcp__x__y"}}'), { kind: 'ApprovalRequested', session: 's1', what: 'run a tool', authorizeTool: 'mcp__x__y' });
});
/**
 * The engine grows variants we do not model. Falling over on one would take the whole
 * stream down — including the plan gate, which is the part that cannot wait.
 */
(0, node_test_1.test)('an unmodelled variant is ignored rather than fatal', () => {
    strict_1.default.equal((0, events_1.parseEngineEvent)('{"AuditAppended":{"session":"s1","summary":"x"}}'), undefined);
    strict_1.default.equal((0, events_1.parseEngineEvent)('not json at all'), undefined);
    strict_1.default.equal((0, events_1.parseEngineEvent)('{"PlanProposed":{"session":"s1"}}'), undefined);
});
(0, node_test_1.test)('only WaitingInput counts as waiting on the operator', () => {
    strict_1.default.equal((0, events_1.statusIsWaiting)('WaitingInput'), true);
    strict_1.default.equal((0, events_1.statusIsWaiting)('Running'), false);
    strict_1.default.equal((0, events_1.statusIsWaiting)('Idle'), false);
});
//# sourceMappingURL=events.test.js.map