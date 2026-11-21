# Companion art provenance

Every image was generated with built-in Codex image generation using
transparent output (`codex exec` with the source image attached). The offline
builder only crops, scales, places, sequences and cross-fades these PNGs. It
never draws a face: the blink and the state poses are whole generated frames,
and each mouth frame is the master with the generated mouth shown only inside
a feathered mouth region (see `REGIONS` in `render.py`).

## reference.png

New character reference.

> Original anime-style character reference sheet for an ambient smart display. Draw a friendly adult woman in her late twenties, fully clothed in a high-collar deep teal jacket with warm cream shoulder panels and a single small cinnamon red cuff; warm brown skin, short indigo wavy bob with one pale teal hair clip, amber eyes. Clean bold cel shading, crisp silhouettes and readable facial features. A two-panel sheet in landscape orientation: left panel is a straight-on waist-up relaxed pose with a gentle shush gesture (index finger just to one side of her lips, mouth visible) and calm open eyes; right panel is matching three-quarter waist-up view with a friendly closed-mouth smile and hands lowered. Leave wide transparent margins around and between the two separate cutouts. No text, grid, logos, set dressing, or existing character likeness. Actual transparent background.

## neutral.png

The master frame, 1024x1536. Edit of the upper-left idle pose of poses.png,
cut out (x 97, y 11, 327x488) and scaled to the canvas width. That atlas was an
earlier edit of reference.png and is no longer kept.

> Redraw this exact original anime character cutout as a crisp full-resolution master on a portrait 2:3 canvas. Keep the SAME adult woman, pose, framing and scale: straight-on waist-up, shoulders square, arms lowered, hair crown just under the top edge, figure horizontally centred, body cut off by the bottom edge at the same waist level, nothing moved, mirrored or re-posed. Calm open amber eyes looking at the viewer, pleasant closed mouth. Warm brown skin, short wavy indigo bob with one pale mint hair clip on viewer-right, high-collar deep teal jacket with warm cream shoulder panels and centre zip, cinnamon red cuffs. Clean bold cel shading, crisp even outlines, flat colours, readable facial features. Actual transparent background with true alpha: no glow, shadow, gradient, halo, text, border or backdrop.

## The mouth shapes, the blink and the state frames

Each is a separate edit of neutral.png with the same prompt and one change. The
five vowel mouths (a, i, u, e, o) are stills for audio-driven lip sync; the
builder does not use them yet.

> Edit this animation frame of an original anime character to make the next frame of the same animation. Keep the image IDENTICAL everywhere except the change described below: the same adult woman, same portrait 2:3 canvas, same framing, scale and position with her hair crown just under the top edge and her body cut at the bottom edge, same outlines, colours and cel shading, same indigo wavy bob with mint hair clip, same teal and cream high-collar jacket with every fold and seam where it is, same actual transparent background with true alpha. Nothing may be moved, resized, re-posed, restyled, mirrored or added. THE ONLY CHANGE: (the change below) No glow, shadow, text, border or backdrop.

- mouth-half.png: her lips are slightly parted in mid speech, showing a small dark gap and a hint of upper teeth, about one third open. Eyes, brows, nose, jaw and face outline stay exactly as they are.
- mouth-open.png: her mouth is open in a relaxed speaking shape, moderately open: no wider than her closed smile and about half as tall as it is wide, showing the edge of the upper teeth and a little tongue. A calm talking mouth, not a shout or a laugh. Only the mouth changes; eyes, brows, nose, cheeks, head position and face outline stay exactly as they are.
- mouth-a.png: her mouth says the vowel 'a' (as in 'father'): a relaxed open oval no wider than her closed smile, the jaw lowered just a little, the edge of the upper teeth visible and the tongue low. A calm speaking mouth, not a shout or a laugh. Only the mouth changes; eyes, brows, nose, cheeks, head position and face outline stay exactly as they are.
- mouth-i.png: her mouth says the vowel 'i' (as in 'see'): lips stretched slightly sideways with a narrow opening, teeth nearly together and showing as a thin light strip. Only the mouth changes; eyes, brows, nose, cheeks, head position and face outline stay exactly as they are.
- mouth-u.png: her mouth says the vowel 'u' (as in 'blue'): lips rounded and gently pursed into a small pucker with a small round dark opening, no teeth showing. Only the mouth changes; eyes, brows, nose, cheeks, head position and face outline stay exactly as they are.
- mouth-e.png: her mouth says the vowel 'e' (as in 'bed'): mouth half open with the lips a little wide, the upper teeth visible, the opening clearly shorter than a full 'a'. Only the mouth changes; eyes, brows, nose, cheeks, head position and face outline stay exactly as they are.
- mouth-o.png: her mouth says the vowel 'o' (as in 'go'): lips rounded into a medium round opening, narrower than her smile, the jaw lowered just a little, no teeth showing. Only the mouth changes; eyes, brows, nose, cheeks, head position and face outline stay exactly as they are.
- blink.png: both eyelids are softly closed in a natural blink, drawn as two calm downward-curved lash lines where the eyes were. Brows, nose, closed smiling mouth and face outline stay exactly as they are.
- listening.png: her HEAD is tilted about 8 degrees toward viewer-right in a curious, attentive listening tilt, with brows raised and eyes slightly wider, looking at the viewer, lips closed in a soft smile. Only the head, hair and face change; the neck, collar, shoulders, arms and whole jacket stay exactly where they are.
- connecting.png: her eyes glance toward viewer-left, as if waiting for something off screen, with her head turned very slightly that way, brows relaxed and lips closed in a small calm smile. Only the head, hair and face change; the neck, collar, shoulders, arms and whole jacket stay exactly where they are.
- thinking.png: she is thinking: her eyes look diagonally up toward viewer-right, her head is tilted a few degrees, one brow is raised and her closed lips are pressed slightly to one side. Only the head, hair and face change; the neck, collar, shoulders, arms and whole jacket stay exactly where they are.
- error.png: she looks worried and apologetic: brows knitted and raised in the middle, eyes on the viewer, a small closed-mouth frown, head lowered a touch. Only the head, hair and face change; the neck, collar, shoulders, arms and whole jacket stay exactly where they are.
- happy.png: she is beaming: both eyes closed in happy upward-curved arcs, a wide open smile showing her upper teeth, cheeks lifted. Only the head, hair and face change; the neck, collar, shoulders, arms and whole jacket stay exactly where they are.
- setup.png: she looks welcoming: her head is turned slightly toward viewer-right and her eyes look to viewer-right, at something beside her, with a friendly small open smile. Only the head, hair and face change; the neck, collar, shoulders, arms and whole jacket stay exactly where they are.
- approval.png: she looks calm and attentive, waiting for an answer: her head is inclined slightly toward viewer-right, her eyes look to viewer-right, brows a little raised in a gentle question, closed-mouth encouraging smile. Only the head, hair and face change; the neck, collar, shoulders, arms and whole jacket stay exactly where they are.
