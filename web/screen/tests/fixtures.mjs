export const states = [
    '{"state":"ready","message":"Echo Live ready"}',
    ...[
        "connecting",
        "connected",
        "listening",
        "thinking",
        "speaking",
        "muted",
    ].map((state) => JSON.stringify({ state })),
    '{"state":"link_code","user_code":"DEMO-CODE","verification_uri":"https://example.invalid/verify","expires_at_unix":1700000000}',
    '{"state":"approval","id":"approval-1","prompt":"Continue?","accept_label":"Allow","deny_label":"Deny"}',
    '{"state":"emotion","name":"happy","frame_ms":80,"frames":["frame-a","frame-b"]}',
    '{"state":"error","message":"Please relink","relink":true}',
    '{"state":"error","message":"Please relink","relink":false}',
];
export const actionWires = [
    '{"action":"tap","target":"primary"}',
    '{"action":"approve","id":"approval-1"}',
    '{"action":"deny","id":"approval-1"}',
    '{"action":"relink"}',
    '{"action":"dismiss"}',
];
export const frame = (seq, state = states[0]) =>
    `{"seq":${seq},"state":${state}}`;
