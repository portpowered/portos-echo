"""Finite native capture input; an overlap never synthesizes another wake."""


def finite_input(host, value, wake_signal):
    data = bytes(value["pcm"])
    if not host.finite_mode or not data or len(data) > 4096 or len(data) % 1600:
        raise ValueError("invalid finite fixture")
    with host.condition:
        if value["action"] == "finite" and host.finite:
            raise ValueError("finite fixture already sent")
        if host.pending:
            raise ValueError("previous finite input is not drained")
        if value["action"] == "muted" and not host.muted:
            raise ValueError("muted input requires native mute acknowledgment")
        if value["action"] == "finite":
            host.finite = True
            host.proc.send_signal(wake_signal)
        elif not host.finite:
            raise ValueError("overlap needs an existing finite run")
        target = host.fed + len(data) + 8192
        if value["action"] == "muted":
            # The finite pipe is 4096 bytes. Writing this tail proves the native
            # capture backend consumed the entire marker while mute was held.
            host.proc.send_signal(wake_signal)
            data += bytes(8192)
        host.pending = data
        host.condition.notify_all()
        if value["action"] == "muted":
            ready = host.condition.wait_for(
                lambda: host.fed >= target or host.error or host.cancelled.is_set(), 20)
            if not ready or host.error or host.cancelled.is_set():
                raise RuntimeError("muted native capture consumption failed")
