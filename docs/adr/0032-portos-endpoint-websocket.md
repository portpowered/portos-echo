---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 21
status: accepted
---

# Port OS endpoint WebSocket transport

The configured bridge uses `tokio-tungstenite` and `tungstenite` for the
Port OS endpoint gateway. The client enables native-root rustls and opens
one authenticated WSS connection per active installation. Gateway frames
are bounded, and the handshake does not follow redirects. The bridge
addresses the gateway with one escaped installation segment and sends
uplink messages through its existing reqwest stack.

The gateway and uplink use the same endpoint-only bearer. The MCP
resource grant stays in its own store and its bearer never enters the
endpoint transport. Production activation requires an authoritative
route resolver. Tests substitute loopback services at the transport edges.

This adds two registry packages to the lockfile and reuses the existing
Tokio, futures-util, and rustls dependencies. Required CI builds the ARMv7
artifact and checks dependencies on the target before release. To roll back,
disable the endpoint or remove this transport and its endpoint-only callers.
