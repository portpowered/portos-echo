//! TCP backpressure for the old peer inside its private loopback namespace.

use std::{net::TcpStream, process::Command};

pub(super) fn limit_send_buffer() {
    checked("ip", &["link", "set", "lo", "mtu", "1500"]);
    checked("sysctl", &["-qw", "net.ipv4.tcp_wmem=4096 4096 4096"]);
}

pub(super) fn drop_acks(peer: &TcpStream) {
    let source = peer.local_addr().unwrap().port().to_string();
    let destination = peer.peer_addr().unwrap().port().to_string();
    checked("tc", &["qdisc", "add", "dev", "lo", "clsact"]);
    // The armed peer sends only TCP acknowledgements during capture. Dropping
    // them prevents partial progress from extending the native write deadline.
    checked(
        "tc",
        &[
            "filter",
            "add",
            "dev",
            "lo",
            "egress",
            "protocol",
            "ip",
            "flower",
            "ip_proto",
            "tcp",
            "src_port",
            &source,
            "dst_port",
            &destination,
            "action",
            "drop",
        ],
    );
}

pub(super) fn restore_acks() {
    checked("tc", &["qdisc", "del", "dev", "lo", "clsact"]);
}

fn checked(program: &str, args: &[&str]) {
    let status = Command::new(program).args(args).status().unwrap();
    assert!(status.success(), "{program} {args:?} exited with {status}");
}
