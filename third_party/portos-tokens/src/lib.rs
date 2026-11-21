// Generated from frontend/packages/ui/theme/tokens.json by frontend/packages/ui/theme/generate-tokens.mjs.
// Do not edit by hand. A consumer repository vendors this crate with its sync script.

//! Port OS design tokens: colors, type, spacing, radii, borders, shadows and breakpoints.
#![no_std]
#![forbid(unsafe_code)]

/// A color as 0xRRGGBBAA, with its hex text (`#rrggbb`, or `#rrggbbaa` when it is not opaque).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub rgba: u32,
    pub hex: &'static str,
}

impl Color {
    pub const fn new(rgba: u32, hex: &'static str) -> Self {
        Self { rgba, hex }
    }

    /// The red, green, blue and alpha channels.
    pub const fn channels(self) -> [u8; 4] {
        self.rgba.to_be_bytes()
    }
}

/// A text role: size, line height and letter spacing in px, and the font weight.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle {
    pub size: f32,
    pub line_height: f32,
    pub weight: u16,
    pub letter_spacing: f32,
}

impl TextStyle {
    pub const fn new(size: f32, line_height: f32, weight: u16, letter_spacing: f32) -> Self {
        Self {
            size,
            line_height,
            weight,
            letter_spacing,
        }
    }
}

/// Colors for the light and dark schemes. Dark lists every light name, with the dark value where one differs.
pub mod color {
    /// The light scheme.
    pub mod light {
        use crate::Color;

        pub const BACKGROUND: Color = Color::new(0xFFFFFFFF, "#ffffff");
        pub const FOREGROUND: Color = Color::new(0x17191DFF, "#17191d");
        pub const CARD: Color = Color::new(0xFFFFFFFF, "#ffffff");
        pub const CARD_FOREGROUND: Color = Color::new(0x2C2C2CFF, "#2c2c2c");
        pub const POPOVER: Color = Color::new(0xFFFFFFFF, "#ffffff");
        pub const POPOVER_FOREGROUND: Color = Color::new(0x2C2C2CFF, "#2c2c2c");
        pub const PRIMARY: Color = Color::new(0xFFDA36FF, "#ffda36");
        pub const PRIMARY_FOREGROUND: Color = Color::new(0x2C2C2CFF, "#2c2c2c");
        pub const SECONDARY: Color = Color::new(0xF7F7F7FF, "#f7f7f7");
        pub const SECONDARY_FOREGROUND: Color = Color::new(0x2C2C2CFF, "#2c2c2c");
        pub const MUTED: Color = Color::new(0xF7F7F7FF, "#f7f7f7");
        pub const MUTED_FOREGROUND: Color = Color::new(0x59616EFF, "#59616e");
        pub const TERTIARY_FOREGROUND: Color = Color::new(0x687080FF, "#687080");
        pub const ACCENT: Color = Color::new(0xCACACAFF, "#cacaca");
        pub const ACCENT_FOREGROUND: Color = Color::new(0x2C2C2CFF, "#2c2c2c");
        pub const DESTRUCTIVE: Color = Color::new(0xF43F5EFF, "#f43f5e");
        pub const DESTRUCTIVE_FOREGROUND: Color = Color::new(0xFCFCFCFF, "#fcfcfc");
        pub const SUCCESS: Color = Color::new(0x22C55EFF, "#22c55e");
        pub const SUCCESS_FOREGROUND: Color = Color::new(0xFCFCFCFF, "#fcfcfc");
        pub const WARNING: Color = Color::new(0xF59E0BFF, "#f59e0b");
        pub const WARNING_FOREGROUND: Color = Color::new(0x2C2C2CFF, "#2c2c2c");
        pub const INFO: Color = Color::new(0x5EB4C2FF, "#5eb4c2");
        pub const INFO_FOREGROUND: Color = Color::new(0xFCFCFCFF, "#fcfcfc");
        pub const BORDER: Color = Color::new(0xF2F2F2FF, "#f2f2f2");
        pub const CODE_PANEL: Color = Color::new(0xF5F6F8FF, "#f5f6f8");
        pub const CODE_PANEL_LINE: Color = Color::new(0xE7E8EBFF, "#e7e8eb");
        pub const HAIRLINE: Color = Color::new(0xE7E8EBFF, "#e7e8eb");
        pub const SCROLLBAR_THUMB: Color = Color::new(0xC5C9D0FF, "#c5c9d0");
        pub const SCROLLBAR_THUMB_HOVER: Color = Color::new(0xA3A9B3FF, "#a3a9b3");
        pub const CODE_PANEL_FOREGROUND: Color = Color::new(0x17191DFF, "#17191d");
        pub const INPUT: Color = Color::new(0x8C8C8CFF, "#8c8c8c");
        pub const RING: Color = Color::new(0x828282FF, "#828282");
        pub const CHART_1: Color = Color::new(0xE76E50FF, "#e76e50");
        pub const CHART_2: Color = Color::new(0x2A9D90FF, "#2a9d90");
        pub const CHART_3: Color = Color::new(0x274754FF, "#274754");
        pub const CHART_4: Color = Color::new(0xE8C468FF, "#e8c468");
        pub const CHART_5: Color = Color::new(0xF4A462FF, "#f4a462");
        pub const NEUTRAL_0: Color = Color::new(0xFFFFFFFF, "#ffffff");
        pub const NEUTRAL_50: Color = Color::new(0xF8FAFCFF, "#f8fafc");
        pub const NEUTRAL_100: Color = Color::new(0xF1F5F9FF, "#f1f5f9");
        pub const NEUTRAL_200: Color = Color::new(0xE2E8F0FF, "#e2e8f0");
        pub const NEUTRAL_300: Color = Color::new(0xCBD5E1FF, "#cbd5e1");
        pub const NEUTRAL_400: Color = Color::new(0x94A3B8FF, "#94a3b8");
        pub const NEUTRAL_500: Color = Color::new(0x64748BFF, "#64748b");
        pub const NEUTRAL_600: Color = Color::new(0x475569FF, "#475569");
        pub const NEUTRAL_700: Color = Color::new(0x334155FF, "#334155");
        pub const NEUTRAL_800: Color = Color::new(0x1E293BFF, "#1e293b");
        pub const NEUTRAL_900: Color = Color::new(0x0F172AFF, "#0f172a");
        pub const TEMP_WARM: Color = Color::new(0xFFB366FF, "#ffb366");
        pub const TEMP_SOFT: Color = Color::new(0xFFD699FF, "#ffd699");
        pub const TEMP_NEUTRAL: Color = Color::new(0xFFF5E6FF, "#fff5e6");
        pub const TEMP_PURE: Color = Color::new(0xFFFFFFFF, "#ffffff");
        pub const TEMP_COOL: Color = Color::new(0xE6F2FFFF, "#e6f2ff");
        pub const PRIMARY_SUBTLE: Color = Color::new(0xFFF9E9FF, "#fff9e9");
        pub const PRIMARY_MUTED: Color = Color::new(0xFBE39BFF, "#fbe39b");
        pub const PRIMARY_DEFAULT: Color = Color::new(0xFFDA36FF, "#ffda36");
        pub const PRIMARY_EMPHASIS: Color = Color::new(0xBB8909FF, "#bb8909");
        pub const PRIMARY_STRONG: Color = Color::new(0x755B00FF, "#755b00");
        pub const BRAND_BLUE: Color = Color::new(0x2563EBFF, "#2563eb");
        pub const LINK: Color = Color::new(0x2563EBFF, "#2563eb");
        pub const LINK_HOVER: Color = Color::new(0x1D4ED8FF, "#1d4ed8");
        pub const DESTRUCTIVE_SUBTLE: Color = Color::new(0xFEF2F2FF, "#fef2f2");
        pub const DESTRUCTIVE_MUTED: Color = Color::new(0xFECACAFF, "#fecaca");
        pub const DESTRUCTIVE_DEFAULT: Color = Color::new(0xDC2626FF, "#dc2626");
        pub const DESTRUCTIVE_EMPHASIS: Color = Color::new(0xB91C1CFF, "#b91c1c");
        pub const DESTRUCTIVE_STRONG: Color = Color::new(0x991B1BFF, "#991b1b");
        pub const SUCCESS_SUBTLE: Color = Color::new(0xF0FDF4FF, "#f0fdf4");
        pub const SUCCESS_MUTED: Color = Color::new(0xBBF7D0FF, "#bbf7d0");
        pub const SUCCESS_DEFAULT: Color = Color::new(0x16A34AFF, "#16a34a");
        pub const SUCCESS_EMPHASIS: Color = Color::new(0x15803DFF, "#15803d");
        pub const SUCCESS_STRONG: Color = Color::new(0x166534FF, "#166534");
        pub const WARNING_SUBTLE: Color = Color::new(0xFFFBEBFF, "#fffbeb");
        pub const WARNING_MUTED: Color = Color::new(0xFDE68AFF, "#fde68a");
        pub const WARNING_DEFAULT: Color = Color::new(0xD97706FF, "#d97706");
        pub const WARNING_EMPHASIS: Color = Color::new(0xB45309FF, "#b45309");
        pub const WARNING_STRONG: Color = Color::new(0x92400EFF, "#92400e");
        pub const INFO_SUBTLE: Color = Color::new(0xF0F9FFFF, "#f0f9ff");
        pub const INFO_MUTED: Color = Color::new(0xBAE6FDFF, "#bae6fd");
        pub const INFO_DEFAULT: Color = Color::new(0x0284C7FF, "#0284c7");
        pub const INFO_EMPHASIS: Color = Color::new(0x0369A1FF, "#0369a1");
        pub const INFO_STRONG: Color = Color::new(0x075985FF, "#075985");
        pub const WHITE: Color = Color::new(0xFFFFFFFF, "#ffffff");
        pub const BLACK: Color = Color::new(0x000000FF, "#000000");
        pub const MEDIA_CANVAS: Color = Color::new(0x000000FF, "#000000");
        pub const OVERLAY: Color = Color::new(0x4E4E4E80, "#4e4e4e80");
        pub const DEVICE_FRAME: Color = Color::new(0xD6D8DCFF, "#d6d8dc");
        pub const DEVICE_GLASS: Color = Color::new(0xF5F5F5FF, "#f5f5f5");
        pub const INPUT_BORDER: Color = Color::new(0xB8B5AAFF, "#b8b5aa");
        pub const BRAND_INK: Color = Color::new(0x17191DFF, "#17191d");
        pub const ON_BRAND_BLUE: Color = Color::new(0xFFFFFFFF, "#ffffff");
        pub const DEVICE_SCREEN: Color = Color::new(0xFFFFFFFF, "#ffffff");
        pub const PORTOS_BRAND_GOLD: Color = Color::new(0xFFDA36FF, "#ffda36");
        pub const PORTOS_BRAND_INK: Color = Color::new(0x2C2C2CFF, "#2c2c2c");
        pub const PORTOS_CONTROL_MUTED: Color = Color::new(0x64748BFF, "#64748b");
        pub const PORTOS_CONTROL_SURFACE: Color = Color::new(0xF1F5F9FF, "#f1f5f9");
        pub const PORTOS_CONTROL_BORDER: Color = Color::new(0xE2E8F0FF, "#e2e8f0");
        pub const PORTOS_CONTROL_FOCUS: Color = Color::new(0x475569FF, "#475569");
        pub const PORTOS_CONTROL_THUMB_BORDER: Color = Color::new(0xFFFFFFFF, "#ffffff");
    }

    /// The dark scheme.
    pub mod dark {
        use crate::Color;

        pub const BACKGROUND: Color = Color::new(0x09090BFF, "#09090b");
        pub const FOREGROUND: Color = Color::new(0xFAFAFAFF, "#fafafa");
        pub const CARD: Color = Color::new(0x09090BFF, "#09090b");
        pub const CARD_FOREGROUND: Color = Color::new(0xFAFAFAFF, "#fafafa");
        pub const POPOVER: Color = Color::new(0x09090BFF, "#09090b");
        pub const POPOVER_FOREGROUND: Color = Color::new(0xFAFAFAFF, "#fafafa");
        pub const PRIMARY: Color = Color::new(0xFFDA36FF, "#ffda36");
        pub const PRIMARY_FOREGROUND: Color = Color::new(0x18181BFF, "#18181b");
        pub const SECONDARY: Color = Color::new(0x27272AFF, "#27272a");
        pub const SECONDARY_FOREGROUND: Color = Color::new(0xFAFAFAFF, "#fafafa");
        pub const MUTED: Color = Color::new(0x27272AFF, "#27272a");
        pub const MUTED_FOREGROUND: Color = Color::new(0xA1A1AAFF, "#a1a1aa");
        pub const TERTIARY_FOREGROUND: Color = Color::new(0x8D8D96FF, "#8d8d96");
        pub const ACCENT: Color = Color::new(0x27272AFF, "#27272a");
        pub const ACCENT_FOREGROUND: Color = Color::new(0xFAFAFAFF, "#fafafa");
        pub const DESTRUCTIVE: Color = Color::new(0xBE123CFF, "#be123c");
        pub const DESTRUCTIVE_FOREGROUND: Color = Color::new(0xFAFAFAFF, "#fafafa");
        pub const SUCCESS: Color = Color::new(0x4ADE80FF, "#4ade80");
        pub const SUCCESS_FOREGROUND: Color = Color::new(0x09090BFF, "#09090b");
        pub const WARNING: Color = Color::new(0xFBBF24FF, "#fbbf24");
        pub const WARNING_FOREGROUND: Color = Color::new(0x09090BFF, "#09090b");
        pub const INFO: Color = Color::new(0x62BFCBFF, "#62bfcb");
        pub const INFO_FOREGROUND: Color = Color::new(0x09090BFF, "#09090b");
        pub const BORDER: Color = Color::new(0x27272AFF, "#27272a");
        pub const CODE_PANEL: Color = Color::new(0x18181BFF, "#18181b");
        pub const CODE_PANEL_LINE: Color = Color::new(0x2E2E33FF, "#2e2e33");
        pub const HAIRLINE: Color = Color::new(0x2E2E33FF, "#2e2e33");
        pub const SCROLLBAR_THUMB: Color = Color::new(0x3F3F46FF, "#3f3f46");
        pub const SCROLLBAR_THUMB_HOVER: Color = Color::new(0x52525BFF, "#52525b");
        pub const CODE_PANEL_FOREGROUND: Color = Color::new(0xFAFAFAFF, "#fafafa");
        pub const INPUT: Color = Color::new(0x71717AFF, "#71717a");
        pub const RING: Color = Color::new(0xD4D4D8FF, "#d4d4d8");
        pub const CHART_1: Color = Color::new(0x2662D9FF, "#2662d9");
        pub const CHART_2: Color = Color::new(0x2EB88AFF, "#2eb88a");
        pub const CHART_3: Color = Color::new(0xE88C30FF, "#e88c30");
        pub const CHART_4: Color = Color::new(0xAF57DBFF, "#af57db");
        pub const CHART_5: Color = Color::new(0xE23670FF, "#e23670");
        pub const NEUTRAL_0: Color = Color::new(0xFFFFFFFF, "#ffffff");
        pub const NEUTRAL_50: Color = Color::new(0xF8FAFCFF, "#f8fafc");
        pub const NEUTRAL_100: Color = Color::new(0xF1F5F9FF, "#f1f5f9");
        pub const NEUTRAL_200: Color = Color::new(0xE2E8F0FF, "#e2e8f0");
        pub const NEUTRAL_300: Color = Color::new(0xCBD5E1FF, "#cbd5e1");
        pub const NEUTRAL_400: Color = Color::new(0x94A3B8FF, "#94a3b8");
        pub const NEUTRAL_500: Color = Color::new(0x64748BFF, "#64748b");
        pub const NEUTRAL_600: Color = Color::new(0x475569FF, "#475569");
        pub const NEUTRAL_700: Color = Color::new(0x334155FF, "#334155");
        pub const NEUTRAL_800: Color = Color::new(0x1E293BFF, "#1e293b");
        pub const NEUTRAL_900: Color = Color::new(0x0F172AFF, "#0f172a");
        pub const TEMP_WARM: Color = Color::new(0xFFB366FF, "#ffb366");
        pub const TEMP_SOFT: Color = Color::new(0xFFD699FF, "#ffd699");
        pub const TEMP_NEUTRAL: Color = Color::new(0xFFF5E6FF, "#fff5e6");
        pub const TEMP_PURE: Color = Color::new(0xFFFFFFFF, "#ffffff");
        pub const TEMP_COOL: Color = Color::new(0xE6F2FFFF, "#e6f2ff");
        pub const PRIMARY_SUBTLE: Color = Color::new(0x3A3110FF, "#3a3110");
        pub const PRIMARY_MUTED: Color = Color::new(0xFBE39BFF, "#fbe39b");
        pub const PRIMARY_DEFAULT: Color = Color::new(0xFFDA36FF, "#ffda36");
        pub const PRIMARY_EMPHASIS: Color = Color::new(0xBB8909FF, "#bb8909");
        pub const PRIMARY_STRONG: Color = Color::new(0x755B00FF, "#755b00");
        pub const BRAND_BLUE: Color = Color::new(0x2563EBFF, "#2563eb");
        pub const LINK: Color = Color::new(0x7AA7FFFF, "#7aa7ff");
        pub const LINK_HOVER: Color = Color::new(0xA9C4FFFF, "#a9c4ff");
        pub const DESTRUCTIVE_SUBTLE: Color = Color::new(0x2A1215FF, "#2a1215");
        pub const DESTRUCTIVE_MUTED: Color = Color::new(0x5C2328FF, "#5c2328");
        pub const DESTRUCTIVE_DEFAULT: Color = Color::new(0xDC2626FF, "#dc2626");
        pub const DESTRUCTIVE_EMPHASIS: Color = Color::new(0xB91C1CFF, "#b91c1c");
        pub const DESTRUCTIVE_STRONG: Color = Color::new(0x991B1BFF, "#991b1b");
        pub const SUCCESS_SUBTLE: Color = Color::new(0x0F2A1AFF, "#0f2a1a");
        pub const SUCCESS_MUTED: Color = Color::new(0x1D5233FF, "#1d5233");
        pub const SUCCESS_DEFAULT: Color = Color::new(0x16A34AFF, "#16a34a");
        pub const SUCCESS_EMPHASIS: Color = Color::new(0x15803DFF, "#15803d");
        pub const SUCCESS_STRONG: Color = Color::new(0x166534FF, "#166534");
        pub const WARNING_SUBTLE: Color = Color::new(0x2D2410FF, "#2d2410");
        pub const WARNING_MUTED: Color = Color::new(0x6B4F12FF, "#6b4f12");
        pub const WARNING_DEFAULT: Color = Color::new(0xD97706FF, "#d97706");
        pub const WARNING_EMPHASIS: Color = Color::new(0xFBBF24FF, "#fbbf24");
        pub const WARNING_STRONG: Color = Color::new(0x92400EFF, "#92400e");
        pub const INFO_SUBTLE: Color = Color::new(0x0C2535FF, "#0c2535");
        pub const INFO_MUTED: Color = Color::new(0x164E6CFF, "#164e6c");
        pub const INFO_DEFAULT: Color = Color::new(0x0284C7FF, "#0284c7");
        pub const INFO_EMPHASIS: Color = Color::new(0x0369A1FF, "#0369a1");
        pub const INFO_STRONG: Color = Color::new(0x075985FF, "#075985");
        pub const WHITE: Color = Color::new(0xFFFFFFFF, "#ffffff");
        pub const BLACK: Color = Color::new(0x000000FF, "#000000");
        pub const MEDIA_CANVAS: Color = Color::new(0x000000FF, "#000000");
        pub const OVERLAY: Color = Color::new(0xFAFAFA80, "#fafafa80");
        pub const DEVICE_FRAME: Color = Color::new(0x334155FF, "#334155");
        pub const DEVICE_GLASS: Color = Color::new(0x1E293BFF, "#1e293b");
        pub const INPUT_BORDER: Color = Color::new(0x71717AFF, "#71717a");
        pub const BRAND_INK: Color = Color::new(0x17191DFF, "#17191d");
        pub const ON_BRAND_BLUE: Color = Color::new(0xFFFFFFFF, "#ffffff");
        pub const DEVICE_SCREEN: Color = Color::new(0xFFFFFFFF, "#ffffff");
        pub const PORTOS_BRAND_GOLD: Color = Color::new(0xFFDA36FF, "#ffda36");
        pub const PORTOS_BRAND_INK: Color = Color::new(0x2C2C2CFF, "#2c2c2c");
        pub const PORTOS_CONTROL_MUTED: Color = Color::new(0xA1A1AAFF, "#a1a1aa");
        pub const PORTOS_CONTROL_SURFACE: Color = Color::new(0x27272AFF, "#27272a");
        pub const PORTOS_CONTROL_BORDER: Color = Color::new(0x3F3F46FF, "#3f3f46");
        pub const PORTOS_CONTROL_FOCUS: Color = Color::new(0xD4D4D8FF, "#d4d4d8");
        pub const PORTOS_CONTROL_THUMB_BORDER: Color = Color::new(0xFFFFFFFF, "#ffffff");
    }
}

/// Font families, weights and text roles (Manrope first; viewport-relative sizes at their maximum).
pub mod font {
    use crate::TextStyle;

    pub const FAMILY: &str = "Manrope, system-ui, sans-serif";
    pub const MONO: &str = "'JetBrains Mono', 'Fira Code', monospace";

    pub const WEIGHT_REGULAR: u16 = 400;
    pub const WEIGHT_MEDIUM: u16 = 500;
    pub const WEIGHT_SEMIBOLD: u16 = 600;
    pub const WEIGHT_BOLD: u16 = 700;
    pub const WEIGHT_EXTRABOLD: u16 = 800;

    pub const PAGE_TITLE: TextStyle = TextStyle::new(28.0, 36.0, 700, -0.5);
    pub const SECTION_HEADING: TextStyle = TextStyle::new(20.0, 28.0, 600, -0.25);
    pub const SUBSECTION_HEADING: TextStyle = TextStyle::new(16.0, 24.0, 600, 0.0);
    pub const BODY: TextStyle = TextStyle::new(14.0, 22.0, 400, 0.0);
    pub const LABEL: TextStyle = TextStyle::new(12.0, 16.0, 500, 0.2);
    pub const HEADLINE: TextStyle = TextStyle::new(38.0, 45.6, 800, -1.9);
    pub const FEATURE: TextStyle = TextStyle::new(52.0, 59.8, 800, -2.08);
    pub const HIGHLIGHT: TextStyle = TextStyle::new(60.0, 67.8, 800, -3.3);
    pub const MICRO_SIZE: f32 = 10.0;
    pub const CAPTION_WEIGHT: u16 = 400;
}

/// The spacing scale and the shared component sizes, in px.
pub mod space {
    pub const SPACE_0: f32 = 0.0;
    pub const SPACE_1: f32 = 4.0;
    pub const SPACE_2: f32 = 8.0;
    pub const SPACE_3: f32 = 12.0;
    pub const SPACE_4: f32 = 16.0;
    pub const SPACE_5: f32 = 20.0;
    pub const SPACE_6: f32 = 24.0;
    pub const SPACE_8: f32 = 32.0;
    pub const SPACE_10: f32 = 40.0;
    pub const SPACE_12: f32 = 48.0;
    pub const SPACE_16: f32 = 64.0;

    pub const CONTROL: f32 = 44.0;
    pub const CONTROL_LG: f32 = 56.0;
    pub const ROW: f32 = 44.0;
    pub const BAND: f32 = 64.0;
    pub const ICON_MD: f32 = 24.0;
    pub const CARD_PADDING: f32 = 24.0;
    pub const CARD_PADDING_COMPACT: f32 = 12.0;
    pub const CARD_GAP: f32 = 16.0;
    pub const CARD_HEADER: f32 = 72.0;
    pub const LAYOUT_MAX: f32 = 1552.0;
}

/// Corner radii, in px.
pub mod radius {
    pub const SM: f32 = 4.0;
    pub const MD: f32 = 6.0;
    pub const LG: f32 = 8.0;
    pub const XL: f32 = 12.0;
    pub const XXL: f32 = 16.0;
    pub const CARD: f32 = 20.0;
    pub const PILL: f32 = 999.0;
}

/// Control border and focus ring geometry, in px, and the disabled control opacity.
pub mod border {
    pub const WIDTH: f32 = 1.0;
    pub const FOCUS_OFFSET: f32 = 4.0;
    pub const DISABLED_OPACITY: f32 = 0.55;
}

/// Elevation shadows as CSS box-shadow values.
pub mod shadow {
    pub const RAISED: &str = "0px 1px 3px 0px #00000014, 0px 1px 2px 0px #0000000a";
    pub const FLOATING: &str = "0px 4px 6px 0px #00000012, 0px 2px 4px 0px #0000000a";
    pub const MODAL: &str = "0px 20px 25px 0px #0000001a, 0px 10px 10px 0px #0000000a";
    pub const HERO: &str = "0px 24px 64px 0px #17191d29";
}

/// Layout breakpoints (minimum widths), in px.
pub mod breakpoint {
    pub const SM: f32 = 640.0;
    pub const MD: f32 = 768.0;
    pub const LG: f32 = 1024.0;
    pub const XL: f32 = 1280.0;
    pub const XXL: f32 = 1440.0;
}

/// The same values as CSS custom properties: light on :root, dark under prefers-color-scheme.
pub const CSS: &str = r##":root {
  color-scheme: light;
  --space-0: 0px;
  --space-1: 4px;
  --space-2: 8px;
  --space-3: 12px;
  --space-4: 16px;
  --space-5: 20px;
  --space-6: 24px;
  --space-8: 32px;
  --space-10: 40px;
  --space-12: 48px;
  --space-16: 64px;
  --color-background: #FFFFFF;
  --color-foreground: #17191d;
  --color-card: #ffffff;
  --color-card-foreground: #2c2c2c;
  --color-popover: #ffffff;
  --color-popover-foreground: #2c2c2c;
  --color-primary: #ffda36;
  --color-primary-foreground: #2c2c2c;
  --color-secondary: #f7f7f7;
  --color-secondary-foreground: #2c2c2c;
  --color-muted: #f7f7f7;
  --color-muted-foreground: #59616e;
  --color-tertiary-foreground: #687080;
  --color-accent: #CACACA;
  --color-accent-foreground: #2c2c2c;
  --color-destructive: #f43f5e;
  --color-destructive-foreground: #FCFCFC;
  --color-success: #22c55e;
  --color-success-foreground: #FCFCFC;
  --color-warning: #f59e0b;
  --color-warning-foreground: #2c2c2c;
  --color-info: #5eb4c2;
  --color-info-foreground: #FCFCFC;
  --color-border: #F2F2F2;
  --color-code-panel: #f5f6f8;
  --color-code-panel-line: #e7e8eb;
  --color-hairline: #e7e8eb;
  --color-scrollbar-thumb: #c5c9d0;
  --color-scrollbar-thumb-hover: #a3a9b3;
  --color-code-panel-foreground: #17191d;
  --color-input: #8C8C8C;
  --color-ring: #828282;
  --color-chart-1: #e76e50;
  --color-chart-2: #2a9d90;
  --color-chart-3: #274754;
  --color-chart-4: #e8c468;
  --color-chart-5: #f4a462;
  --radius: 0.5rem;
  --radius-lg: 0.5rem;
  --radius-md: 6px;
  --radius-sm: 4px;
  --color-neutral-0: #FFFFFF;
  --color-neutral-50: #F8FAFC;
  --color-neutral-100: #F1F5F9;
  --color-neutral-200: #E2E8F0;
  --color-neutral-300: #CBD5E1;
  --color-neutral-400: #94A3B8;
  --color-neutral-500: #64748B;
  --color-neutral-600: #475569;
  --color-neutral-700: #334155;
  --color-neutral-800: #1E293B;
  --color-neutral-900: #0F172A;
  --color-temp-warm: #ffb366;
  --color-temp-soft: #ffd699;
  --color-temp-neutral: #fff5e6;
  --color-temp-pure: #ffffff;
  --color-temp-cool: #e6f2ff;
  --font-ui: Manrope, system-ui, sans-serif;
  --font-mono: 'JetBrains Mono', 'Fira Code', monospace;
  --shadow-raised: 0px 1px 3px 0px #00000014, 0px 1px 2px 0px #0000000a;
  --shadow-floating: 0px 4px 6px 0px #00000012, 0px 2px 4px 0px #0000000a;
  --shadow-modal: 0px 20px 25px 0px #0000001a, 0px 10px 10px 0px #0000000a;
  --color-primary-subtle: #fff9e9;
  --color-primary-muted: #fbe39b;
  --color-primary-default: #ffda36;
  --color-primary-emphasis: #bb8909;
  --color-primary-strong: #755b00;
  --color-brand-blue: #2563eb;
  --color-link: #2563eb;
  --color-link-hover: #1d4ed8;
  --color-destructive-subtle: #FEF2F2;
  --color-destructive-muted: #FECACA;
  --color-destructive-default: #DC2626;
  --color-destructive-emphasis: #B91C1C;
  --color-destructive-strong: #991B1B;
  --color-success-subtle: #F0FDF4;
  --color-success-muted: #BBF7D0;
  --color-success-default: #16A34A;
  --color-success-emphasis: #15803D;
  --color-success-strong: #166534;
  --color-warning-subtle: #FFFBEB;
  --color-warning-muted: #FDE68A;
  --color-warning-default: #D97706;
  --color-warning-emphasis: #B45309;
  --color-warning-strong: #92400E;
  --color-info-subtle: #F0F9FF;
  --color-info-muted: #BAE6FD;
  --color-info-default: #0284C7;
  --color-info-emphasis: #0369A1;
  --color-info-strong: #075985;
  --color-white: #ffffff;
  --color-black: #000000;
  --text-page-title-size: 28px;
  --text-page-title-weight: 700;
  --text-page-title-leading: 36px;
  --text-page-title-tracking: -0.5px;
  --text-section-heading-size: 20px;
  --text-section-heading-weight: 600;
  --text-section-heading-leading: 28px;
  --text-section-heading-tracking: -0.25px;
  --text-subsection-heading-size: 16px;
  --text-subsection-heading-weight: 600;
  --text-subsection-heading-leading: 24px;
  --text-subsection-heading-tracking: 0px;
  --text-body-size: 14px;
  --text-body-weight: 400;
  --text-body-leading: 22px;
  --text-body-tracking: 0px;
  --text-label-size: 12px;
  --text-label-weight: 500;
  --text-label-leading: 16px;
  --text-label-tracking: 0.2px;
  --text-body-strong-weight: 600;
  --text-caption-weight: 400;
  --text-code-size: 13px;
  --font-sans: Manrope, system-ui, sans-serif;
  --color-media-canvas: #000000;
  --color-overlay: rgba(78, 78, 78, 0.5);
  --radius-xl: 12px;
  --radius-2xl: 16px;
  --radius-card: 20px;
  --radius-pill: 999px;
  --radius-bubble-sent: 18px 18px 5px 18px;
  --radius-bubble-received: 18px 18px 18px 5px;
  --color-device-frame: #d6d8dc;
  --color-device-glass: #f5f5f5;
  --color-input-border: #b8b5aa;
  --color-brand-ink: #17191d;
  --shadow-hero: 0px 24px 64px 0px #17191d29;
  --text-headline-size: clamp(28px, 3vw, 38px);
  --text-headline-weight: 800;
  --text-headline-leading: 1.2;
  --text-headline-tracking: -0.05em;
  --text-feature-size: clamp(32px, 4vw, 52px);
  --text-feature-weight: 800;
  --text-feature-leading: 1.15;
  --text-feature-tracking: -0.04em;
  --text-highlight-size: clamp(34px, 4.4vw, 60px);
  --text-highlight-weight: 800;
  --text-highlight-leading: 1.13;
  --text-highlight-tracking: -0.055em;
  --color-on-brand-blue: #ffffff;
  --color-device-screen: #ffffff;
  --portos-brand-gold: #ffda36;
  --portos-brand-ink: #2c2c2c;
  --portos-control-muted: #64748b;
  --portos-control-surface: #f1f5f9;
  --portos-control-border: #e2e8f0;
  --portos-control-focus: #475569;
  --portos-control-thumb-border: white;
  --portos-control-dark-text: #fafafa;
  --portos-control-dark-muted: #a1a1aa;
  --portos-control-dark-surface: #27272a;
  --portos-control-dark-border: #3f3f46;
  --portos-control-dark-focus: #d4d4d8;
  --portos-power-size: 144px;
  --portos-power-icon-size: 60px;
  --portos-power-radius: 50%;
  --portos-range-height: 8px;
  --portos-range-radius: 999px;
  --portos-range-thumb-size: 24px;
  --portos-range-moz-thumb-size: 18px;
  --portos-control-border-size: 1px;
  --portos-control-focus-offset: 4px;
  --portos-control-disabled-opacity: .55;
  --portos-action-disabled-opacity: .7;
  --portos-control-hover-brightness: .96;
  --portos-power-elevation: 0px 8px 28px 0px #f4ba1d3d;
  --portos-range-elevation: 0px 2px 8px 0px #0002;
  --state-shell-radius: 0.5rem;
  --state-success-text: #16A34A;
  --state-success-border: #16a34a33;
  --state-success-background: #16a34a1a;
  --state-error-text: #DC2626;
  --state-error-border: #dc262633;
  --state-error-background: #dc26261a;
  --state-warning-text: #D97706;
  --state-warning-border: #d9770633;
  --state-warning-background: #d977061a;
  --state-info-text: #0369A1;
  --state-info-border: #0284c733;
  --state-info-background: #0284c71a;
  --state-muted-text: #64748B;
  --state-muted-border: #E2E8F0;
  --state-muted-background: #F8FAFC;
  --text-hero: clamp(42px, 4.5vw, 64px);
  --text-hero-phone: clamp(34px, 9.5vw, 54px);
  --color-area-height: 128px;
  --color-area-thumb-size: 16px;
  --text-display: clamp(32px, 4vw, 48px);
  --text-prominent: clamp(24px, 3vw, 38px);
  --spacing-control: 44px;
  --spacing-control-lg: 56px;
  --spacing-row: 44px;
  --spacing-band: 64px;
  --spacing-icon-md: 24px;
  --spacing-card-padding: 24px;
  --spacing-card-padding-compact: 12px;
  --spacing-card-gap: 16px;
  --spacing-card-header: 72px;
  --radius-tile: 28%;
  --text-lead: 18px;
  --text-micro: 10px;
  --text-intro: clamp(1rem, 1.7vw, 1.25rem);
  --text-giant: clamp(6rem, 11vw, 9rem);
  --font-weight-regular: 400;
  --font-weight-medium: 500;
  --font-weight-semibold: 600;
  --font-weight-bold: 700;
  --font-weight-extrabold: 800;
  --text-hero--line-height: 1.08;
  --text-hero--letter-spacing: -0.065em;
  --text-hero--font-weight: 800;
  --text-hero-phone--line-height: 1.08;
  --text-hero-phone--letter-spacing: -0.065em;
  --text-hero-phone--font-weight: 800;
  --text-display--line-height: 1.1;
  --text-prominent--line-height: 1.15;
  --portos-sheet-enter-duration: .3s;
  --portos-sheet-enter-ease: cubic-bezier(.32,.72,0,1);
  --portos-sheet-exit-duration: .2s;
  --portos-sheet-exit-ease: ease-in;
  --breakpoint-sm: 640px;
  --breakpoint-md: 768px;
  --breakpoint-lg: 1024px;
  --breakpoint-xl: 1280px;
  --breakpoint-2xl: 1440px;
  --font-ui: var(--font-sans);
}

@media (prefers-color-scheme: dark) {
  :root {
    color-scheme: dark;
    --portos-control-muted: #a1a1aa;
    --portos-control-surface: #27272a;
    --portos-control-border: #3f3f46;
    --portos-control-focus: #d4d4d8;
    --color-background: #09090b;
    --color-brand-blue: #2563eb;
    --color-link: #7aa7ff;
    --color-link-hover: #a9c4ff;
    --color-foreground: #fafafa;
    --color-card: #09090b;
    --color-card-foreground: #fafafa;
    --color-popover: #09090b;
    --color-popover-foreground: #fafafa;
    --color-primary: #ffda36;
    --color-primary-foreground: #18181b;
    --color-secondary: #27272a;
    --color-secondary-foreground: #fafafa;
    --color-muted: #27272a;
    --color-muted-foreground: #a1a1aa;
    --color-tertiary-foreground: #8d8d96;
    --color-accent: #27272a;
    --color-accent-foreground: #fafafa;
    --color-destructive: #be123c;
    --color-destructive-foreground: #fafafa;
    --color-success: #4ade80;
    --color-success-foreground: #09090b;
    --color-warning: #fbbf24;
    --color-warning-foreground: #09090b;
    --color-warning-emphasis: #fbbf24;
    --color-info: #62bfcb;
    --color-info-foreground: #09090b;
    --color-border: #27272a;
    --color-destructive-subtle: #2a1215;
    --color-destructive-muted: #5c2328;
    --color-success-subtle: #0f2a1a;
    --color-success-muted: #1d5233;
    --color-warning-subtle: #2d2410;
    --color-warning-muted: #6b4f12;
    --color-info-subtle: #0c2535;
    --color-info-muted: #164e6c;
    --color-primary-subtle: #3a3110;
    --color-code-panel: #18181b;
    --color-code-panel-line: #2e2e33;
    --color-hairline: #2e2e33;
    --color-scrollbar-thumb: #3f3f46;
    --color-scrollbar-thumb-hover: #52525b;
    --color-code-panel-foreground: #fafafa;
    --color-input: #71717a;
    --color-ring: #d4d4d8;
    --color-chart-1: #2662d9;
    --color-chart-2: #2eb88a;
    --color-chart-3: #e88c30;
    --color-chart-4: #af57db;
    --color-chart-5: #e23670;
    --color-media-canvas: #000000;
    --color-overlay: rgba(250, 250, 250, 0.5);
    --color-device-frame: #334155;
    --color-device-glass: #1E293B;
    --color-input-border: #71717a;
  }
}
"##;
