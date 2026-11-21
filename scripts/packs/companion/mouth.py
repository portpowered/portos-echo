"""Stills for audio-driven lip sync: one whole frame per mouth shape."""

from render import ART, MASTER, raster, raw_input, run

# The neutral frame, already published as rest.png, is the closed mouth.
CLOSED = "rest.png"
REQUIRED = ("half", "open")
# A vowel shape is published when its art exists; the pack works without any.
VOWELS = ("a", "i", "u", "e", "o")


def stills(pack):
    """Write the mouth stills into the pack and return its manifest mouth set.

    Call after render(): the stills take the same placement and colour grade
    as the neutral frame, so they register with the speaking clip they cover.
    """
    mouth = {"closed": CLOSED}
    shapes = [*REQUIRED, *(v for v in VOWELS if (ART / f"mouth-{v}.png").is_file())]
    for shape in shapes:
        name = f"mouth-{shape}.png"
        run([*raw_input("pipe:0"), "-frames:v", "1", "-threads", "1", str(pack / name)],
            raster(f"mouth-{shape}", MASTER))
        mouth[shape] = name
    return mouth
