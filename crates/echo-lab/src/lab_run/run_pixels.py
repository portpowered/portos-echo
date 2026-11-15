"""Bounded lossless PNG decoder for Android's non-interlaced 8-bit screencaps."""
import struct
import zlib


def png_chunks(data):
    if not data.startswith(b"\x89PNG\r\n\x1a\n"):
        raise ValueError("not PNG")
    pos = 8
    while pos < len(data):
        size = struct.unpack(">I", data[pos:pos + 4])[0]
        kind = data[pos + 4:pos + 8]
        payload = data[pos + 8:pos + 8 + size]
        crc = data[pos + 8 + size:pos + 12 + size]
        if len(payload) != size or crc != struct.pack(">I", zlib.crc32(kind + payload)):
            raise ValueError("PNG checksum")
        yield kind, payload
        pos += size + 12
        if kind == b"IEND":
            if payload or pos != len(data):
                raise ValueError("PNG trailing data")
            return
    raise ValueError("PNG missing IEND")


def unfilter(raw, width, height, channels):
    stride = width * channels
    if len(raw) != (stride + 1) * height:
        raise ValueError("PNG size")
    previous = bytearray(stride)
    for row in range(height):
        start = row * (stride + 1)
        kind = raw[start]
        current = bytearray(raw[start + 1:start + stride + 1])
        for index in range(stride):
            left = current[index - channels] if index >= channels else 0
            above = previous[index]
            upper = previous[index - channels] if index >= channels else 0
            estimate = left + above - upper
            distances = [abs(estimate - v) for v in (left, above, upper)]
            paeth = (left, above, upper)[distances.index(min(distances))]
            predictors = (0, left, above, (left + above) // 2, paeth)
            if kind > 4:
                raise ValueError("PNG filter")
            current[index] = (current[index] + predictors[kind]) & 255
        yield current
        previous = current


def decode_png(path):
    with open(path, "rb") as source:
        data = source.read(16 * 1024 * 1024 + 1)
    if len(data) > 16 * 1024 * 1024:
        raise ValueError("PNG input limit")
    chunks = list(png_chunks(data))
    if not chunks or chunks[0][0] != b"IHDR":
        raise ValueError("PNG header")
    width, height, depth, color, compression, filtering, interlace = struct.unpack(">IIBBBBB", chunks[0][1])
    channels = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}.get(color)
    if not channels or depth != 8 or compression or filtering or interlace or not width or not height:
        raise ValueError("unsupported PNG format")
    limit = (width * channels + 1) * height
    if limit > 32 * 1024 * 1024:
        raise ValueError("PNG decoded limit")
    compressed = b"".join(payload for kind, payload in chunks if kind == b"IDAT")
    decoder = zlib.decompressobj()
    raw = decoder.decompress(compressed, limit + 1)
    if len(raw) != limit or not decoder.eof or decoder.unused_data:
        raise ValueError("PNG compression")
    palette = next((p for k, p in chunks if k == b"PLTE"), b"")
    alpha = next((p for k, p in chunks if k == b"tRNS"), b"")
    pixels = bytearray()
    for row in unfilter(raw, width, height, channels):
        for index in range(0, len(row), channels):
            sample = row[index:index + channels]
            if color == 6:
                pixels.extend(sample)
            elif color == 2:
                transparent = bool(alpha) and tuple(sample) == struct.unpack(">HHH", alpha)
                pixels.extend(sample + bytes([0 if transparent else 255]))
            elif color == 3:
                offset = sample[0] * 3
                if offset + 3 > len(palette):
                    raise ValueError("PNG palette")
                pixels.extend(palette[offset:offset + 3] + bytes([alpha[sample[0]] if sample[0] < len(alpha) else 255]))
            else:
                opacity = sample[1] if color == 4 else 255
                if color == 0 and alpha and sample[0] == struct.unpack(">H", alpha)[0]:
                    opacity = 0
                pixels.extend(bytes([sample[0]]) * 3 + bytes([opacity]))
    return width, height, pixels


def compare_pixels(captured, golden):
    actual, expected = decode_png(captured), decode_png(golden)
    if actual[:2] != expected[:2]:
        return {"dimensions_match": False}
    count = sum(actual[2][i:i + 4] != expected[2][i:i + 4] for i in range(0, len(actual[2]), 4))
    return {"different_pixels": count}
