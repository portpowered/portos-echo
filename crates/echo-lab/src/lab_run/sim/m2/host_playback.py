"""Independent classification of the finite synthetic host speaker recording."""
import math
import struct


def playback_counts(data, earcon=False):
    original = data
    if earcon:
        if len(data) < 7680:
            raise ValueError("incomplete host earcon")
        for index, (sample,) in enumerate(struct.iter_unpack("<h", data[:7680])):
            position = index % 1920
            edge = min(position, 1919 - position, 240)
            fade = (1 - math.cos(math.pi * edge / 240)) / 2
            frequency = 750 if index < 1920 else 1000
            expected = 983 * fade * math.sin(math.tau * frequency * position / 24000)
            if abs(sample - expected) > 1:
                raise ValueError("corrupt host earcon")
        data = data[7680:]
    result = {"event": "host.ack", "pcm_bytes": len(data),
              "nonzero_bytes": sum(byte != 0 for byte in data)}
    if earcon:
        result.update(total_pcm_bytes=len(original), earcon_pcm_bytes=7680,
                      earcon_nonzero_bytes=sum(byte != 0 for byte in original[:7680]))
    return result
