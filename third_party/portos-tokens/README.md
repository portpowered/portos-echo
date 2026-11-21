<!-- Generated from frontend/packages/ui/theme/tokens.json by frontend/packages/ui/theme/generate-tokens.mjs. Do not edit by hand. -->
# portos-tokens

The Port OS design tokens as a dependency-free Rust crate, for native or embedded surfaces that should match the Port OS UI.

- `color::light` and `color::dark`: `Color { rgba, hex }`, with `rgba` as `0xRRGGBBAA`.
- `font`: `FAMILY` (Manrope first), `MONO`, the weights, and the text roles as `TextStyle` in px.
- `space`, `radius`, `border` and `breakpoint`: lengths in px as `f32`. `shadow`: CSS `box-shadow` values.
- `CSS`: the same values as `:root` custom properties, with the dark scheme under `prefers-color-scheme: dark`.

Viewport-relative sizes (`clamp()`) resolve to their maximum.

## Font

The Port OS UI uses one face: Manrope, variable weight 200 to 800, Latin subset (`manrope-latin-wght-normal.woff2` from `@fontsource-variable/manrope` 5.3.0). Manrope is licensed under the SIL Open Font License 1.1. This crate does not bundle it: serve the woff2 with an `@font-face` rule for `font-family: Manrope` and `font-weight: 200 800`.

## Source and updates

This crate is generated from the Port OS design token source and published to [portpowered/portos-ui-rust](https://github.com/portpowered/portos-ui-rust) as a release copy: each commit there names the Port OS source commit it copies. Do not edit the files by hand, because the next release overwrites them; to vendor it, copy `Cargo.toml`, `src/lib.rs` and `README.md` unchanged.

## License

Apache-2.0. Manrope is not included and keeps its own license (SIL Open Font License 1.1).
