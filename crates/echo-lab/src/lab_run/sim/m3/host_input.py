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


def flush_observation(host, wait=False):
    """Native worker receipt/close and independent discarded PCM, never relay estimates."""
    import re
    with host.condition:
        def complete():
            return re.findall(rb"duplex flush:[^\n]*\n", host.log)
        if wait:
            ready = host.condition.wait_for(
                lambda: complete() or host.error or host.cancelled.is_set(), 5)
            if host.cancelled.is_set():
                from wsl_host import HostCancelled
                raise HostCancelled("host command cancelled")
            if not ready or host.error:
                raise RuntimeError("native flush observation readiness failed")
        lines = complete()
        if not lines and not wait:
            return None
        records = re.findall(rb"duplex flush: receipt_ms=(\d+) closed_ms=(\d+) before=(\d+) written=(\d+) discarded=(\d+)\n", host.log)
        if len(lines) != 1 or len(records) != 1 or host.log.count(b"duplex flush:") != 1:
            raise ValueError("expected one native flush observation")
    keys = ("receipt_ms", "closed_ms", "before", "written", "discarded")
    return dict(zip(keys, map(int, records[0])))


def playback_observation(host, limit, counts, wait=False, earcon=False):
    """Observe native flush readiness before taking the bounded PCM snapshot."""
    flush = flush_observation(host, wait=wait)
    with (host.folder / "play.raw").open("rb") as recording:
        data = recording.read(limit + 1)
    if len(data) > limit or len(data) % 2:
        raise ValueError("invalid playback observation")
    result = counts(data, earcon)
    result["flush"] = flush
    return result
