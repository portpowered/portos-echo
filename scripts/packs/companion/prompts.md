# Companion art provenance

Every image was generated with built-in Codex image generation using
transparent output (`codex exec` with the source image attached). The offline
builder only crops, scales, places, sequences and cross-fades these PNGs. It
never edits a face: each mouth shape, the blink and the Listening head are
whole generated frames.

## reference.png

New character reference.

> Original anime-style character reference sheet for an ambient smart display. Draw a friendly adult woman in her late twenties, fully clothed in a high-collar deep teal jacket with warm cream shoulder panels and a single small cinnamon red cuff; warm brown skin, short indigo wavy bob with one pale teal hair clip, amber eyes. Clean bold cel shading, crisp silhouettes and readable facial features. A two-panel sheet in landscape orientation: left panel is a straight-on waist-up relaxed pose with a gentle shush gesture (index finger just to one side of her lips, mouth visible) and calm open eyes; right panel is matching three-quarter waist-up view with a friendly closed-mouth smile and hands lowered. Leave wide transparent margins around and between the two separate cutouts. No text, grid, logos, set dressing, or existing character likeness. Actual transparent background.

## poses.png

Edit of reference.png.

> Edit this original character reference for an animation sprite atlas. Keep the SAME adult woman, proportions, brown skin, wavy indigo bob, teal hair clip, amber eyes, high-collar teal-and-cream jacket, cinnamon cuff and clean cel shading. Draw exactly six equally spaced separate WAIST-UP front or shallow three-quarter cutouts on a wide landscape canvas, precise 3 columns by 2 rows, each with clear transparent margins, true alpha, complete hair and arms within its own cell. Every cutout should have consistent head height and waist-level crop with arms resting unless requested. Upper left: relaxed idle facing front, hands lowered, open eyes, pleasant closed mouth. Upper center: connecting, head tilted slightly and gaze to one side. Upper right: listening, leaning forward slightly, wide attentive eyes. Lower left: thinking, gazing diagonally up, hand to chin, with three small pale mint thinking dots near her temple. Lower center: worried, brows knitted and small apologetic frown. Lower right: happy, beaming warm smile, eyes crinkled, shoulders perked upward. Distinct readable expressions, restrained movement. No lettering, guides, lines, props, borders, or background.

## neutral.png

The master frame, 1024x1536. Edit of the upper-left idle pose of poses.png,
cut out (x 97, y 11, 327x488) and scaled to the canvas width.

> Redraw this exact original anime character cutout as a crisp full-resolution master on a portrait 2:3 canvas. Keep the SAME adult woman, pose, framing and scale: straight-on waist-up, shoulders square, arms lowered, hair crown just under the top edge, figure horizontally centred, body cut off by the bottom edge at the same waist level, nothing moved, mirrored or re-posed. Calm open amber eyes looking at the viewer, pleasant closed mouth. Warm brown skin, short wavy indigo bob with one pale mint hair clip on viewer-right, high-collar deep teal jacket with warm cream shoulder panels and centre zip, cinnamon red cuffs. Clean bold cel shading, crisp even outlines, flat colours, readable facial features. Actual transparent background with true alpha: no glow, shadow, gradient, halo, text, border or backdrop.

## mouth-half.png, mouth-open.png, blink.png, listening.png

Each is a separate edit of neutral.png with the same prompt and one change.

> Edit this animation frame of an original anime character to make the next frame of the same animation. Keep the image IDENTICAL everywhere except the change described below: the same adult woman, same portrait 2:3 canvas, same framing, scale and position with her hair crown just under the top edge and her body cut at the bottom edge, same outlines, colours and cel shading, same indigo wavy bob with mint hair clip, same teal and cream high-collar jacket with every fold and seam where it is, same actual transparent background with true alpha. Nothing may be moved, resized, re-posed, restyled, mirrored or added. THE ONLY CHANGE: (the change below) No glow, shadow, text, border or backdrop.

- mouth-half.png: her lips are slightly parted in mid speech, showing a small dark gap and a hint of upper teeth, about one third open. Eyes, brows, nose, jaw and face outline stay exactly as they are.
- mouth-open.png: her mouth is open in a rounded speaking shape (an 'ah' vowel), clearly taller than a slightly parted mouth, showing upper teeth and tongue. Eyes, brows, nose, jaw and face outline stay exactly as they are.
- blink.png: both eyelids are softly closed in a natural blink, drawn as two calm downward-curved lash lines where the eyes were. Brows, nose, closed smiling mouth and face outline stay exactly as they are.
- listening.png: her HEAD is tilted about 8 degrees toward viewer-right in a curious, attentive listening tilt, with brows raised and eyes slightly wider, looking at the viewer, lips closed in a soft smile. Only the head, hair and face change; the neck, collar, shoulders, arms and whole jacket stay exactly where they are.

## cards.png

Edit of reference.png.

> Edit the original reference character into a transparent two-cell waist-up presentation atlas. Same adult woman, warm brown skin, short dark indigo wavy bob with mint clip, amber eyes, cream-paneled high-neck teal jacket with rust-red cuffs, stylized clean anime cel shading. Keep the cutouts straight-on or shallow three-quarter, head and waist at matching heights and generous transparent margin between them. Left pose (setup): relaxed, looking slightly to the left, with one small open-palm presenting gesture kept inside her silhouette area. Right pose (approval): calm and attentive, looking straight forward, closed-mouth encouraging smile and hands gently folded near her waist. Make exact transparent alpha outside both silhouettes, no gradients, shadow, glow, border, lettering or logos. Both have clear features at reduced size.
