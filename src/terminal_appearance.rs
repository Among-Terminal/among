//! Pure terminal appearance data: stable identifiers, the pinned catalogue,
//! validated colours and per-dimension resolution.
//!
//! The catalogue is transcribed from GNOME Terminal 3.56.2
//! `src/profile-editor.cc` at commit
//! `941d51d151c0e25b285ffe98ad4a38b4ce5dda69`. The independent fixture at
//! `tests/fixtures/gnome-terminal-3.56.2-colours.json` carries the same
//! published bytes for comparison. Nothing here depends on GTK, GIO, VTE or a
//! running desktop, and inherited values never pass through an integer channel.

use std::fmt;
use std::str::FromStr;

/// A colour with validated finite channels in the inclusive `0.0..=1.0` range.
///
/// Channels stay `f32` so inherited `GdkRGBA` values keep their precision; only
/// the pinned catalogue constants originate as byte triples.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    red: f32,
    green: f32,
    blue: f32,
    alpha: f32,
}

/// A channel that is not finite or does not lie in `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidColour;

impl fmt::Display for InvalidColour {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a colour channel is not finite or is outside 0.0..=1.0")
    }
}

impl std::error::Error for InvalidColour {}

impl Rgba {
    /// Build a colour, rejecting non-finite and out-of-range channels.
    pub fn new(red: f32, green: f32, blue: f32, alpha: f32) -> Result<Self, InvalidColour> {
        let usable = |channel: f32| channel.is_finite() && (0.0..=1.0).contains(&channel);
        if !usable(red) || !usable(green) || !usable(blue) || !usable(alpha) {
            return Err(InvalidColour);
        }
        Ok(Self {
            red,
            green,
            blue,
            alpha,
        })
    }

    /// Build an opaque colour, rejecting non-finite and out-of-range channels.
    pub fn opaque(red: f32, green: f32, blue: f32) -> Result<Self, InvalidColour> {
        Self::new(red, green, blue, 1.0)
    }

    pub fn red(self) -> f32 {
        self.red
    }

    pub fn green(self) -> f32 {
        self.green
    }

    pub fn blue(self) -> f32 {
        self.blue
    }

    pub fn alpha(self) -> f32 {
        self.alpha
    }
}

/// The default text colour and background that belong together.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextBackgroundPair {
    pub foreground: Rgba,
    pub background: Rgba,
}

const fn rgb(red: u8, green: u8, blue: u8) -> Rgba {
    Rgba {
        red: red as f32 / 255.0,
        green: green as f32 / 255.0,
        blue: blue as f32 / 255.0,
        alpha: 1.0,
    }
}

const fn pair(foreground: [u8; 3], background: [u8; 3]) -> TextBackgroundPair {
    TextBackgroundPair {
        foreground: rgb(foreground[0], foreground[1], foreground[2]),
        background: rgb(background[0], background[1], background[2]),
    }
}

const GRAY_ON_BLACK: TextBackgroundPair = pair([0xaa, 0xaa, 0xaa], [0x00, 0x00, 0x00]);

/// A stable text/background choice, including the inherited source row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum TextBackgroundId {
    #[default]
    GnomeTerminalDefault,
    BlackOnLightYellow,
    BlackOnWhite,
    GrayOnBlack,
    GreenOnBlack,
    WhiteOnBlack,
    GnomeLight,
    GnomeDark,
    TangoLight,
    TangoDark,
    SolarizedLight,
    SolarizedDark,
}

impl TextBackgroundId {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::GnomeTerminalDefault => "gnome-terminal-default",
            Self::BlackOnLightYellow => "black-on-light-yellow",
            Self::BlackOnWhite => "black-on-white",
            Self::GrayOnBlack => "gray-on-black",
            Self::GreenOnBlack => "green-on-black",
            Self::WhiteOnBlack => "white-on-black",
            Self::GnomeLight => "gnome-light",
            Self::GnomeDark => "gnome-dark",
            Self::TangoLight => "tango-light",
            Self::TangoDark => "tango-dark",
            Self::SolarizedLight => "solarized-light",
            Self::SolarizedDark => "solarized-dark",
        }
    }
}

impl FromStr for TextBackgroundId {
    type Err = UnknownPreset;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "gnome-terminal-default" => Ok(Self::GnomeTerminalDefault),
            "black-on-light-yellow" => Ok(Self::BlackOnLightYellow),
            "black-on-white" => Ok(Self::BlackOnWhite),
            "gray-on-black" => Ok(Self::GrayOnBlack),
            "green-on-black" => Ok(Self::GreenOnBlack),
            "white-on-black" => Ok(Self::WhiteOnBlack),
            "gnome-light" => Ok(Self::GnomeLight),
            "gnome-dark" => Ok(Self::GnomeDark),
            "tango-light" => Ok(Self::TangoLight),
            "tango-dark" => Ok(Self::TangoDark),
            "solarized-light" => Ok(Self::SolarizedLight),
            "solarized-dark" => Ok(Self::SolarizedDark),
            _ => Err(UnknownPreset),
        }
    }
}

impl fmt::Display for TextBackgroundId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A stable ANSI palette choice, including the inherited source row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum AnsiPaletteId {
    #[default]
    GnomeTerminalDefault,
    Gnome,
    Tango,
    LinuxConsole,
    Xterm,
    Rxvt,
    Solarized,
}

impl AnsiPaletteId {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::GnomeTerminalDefault => "gnome-terminal-default",
            Self::Gnome => "gnome",
            Self::Tango => "tango",
            Self::LinuxConsole => "linux-console",
            Self::Xterm => "xterm",
            Self::Rxvt => "rxvt",
            Self::Solarized => "solarized",
        }
    }
}

impl FromStr for AnsiPaletteId {
    type Err = UnknownPreset;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "gnome-terminal-default" => Ok(Self::GnomeTerminalDefault),
            "gnome" => Ok(Self::Gnome),
            "tango" => Ok(Self::Tango),
            "linux-console" => Ok(Self::LinuxConsole),
            "xterm" => Ok(Self::Xterm),
            "rxvt" => Ok(Self::Rxvt),
            "solarized" => Ok(Self::Solarized),
            _ => Err(UnknownPreset),
        }
    }
}

impl fmt::Display for AnsiPaletteId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A stored identifier that names no known choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownPreset;

impl fmt::Display for UnknownPreset {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("unknown terminal appearance preset")
    }
}

impl std::error::Error for UnknownPreset {}

/// One built-in text/background scheme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextBackgroundPreset {
    pub id: TextBackgroundId,
    pub label: &'static str,
    pub pair: TextBackgroundPair,
}

/// The eleven built-in text/background schemes in upstream order.
///
/// The inherited `GnomeTerminalDefault` source row is deliberately absent.
pub const TEXT_BACKGROUND_CATALOGUE: [TextBackgroundPreset; 11] = [
    TextBackgroundPreset {
        id: TextBackgroundId::BlackOnLightYellow,
        label: "Black on light yellow",
        pair: pair([0x00, 0x00, 0x00], [0xff, 0xff, 0xdd]),
    },
    TextBackgroundPreset {
        id: TextBackgroundId::BlackOnWhite,
        label: "Black on white",
        pair: pair([0x00, 0x00, 0x00], [0xff, 0xff, 0xff]),
    },
    TextBackgroundPreset {
        id: TextBackgroundId::GrayOnBlack,
        label: "Gray on black",
        pair: GRAY_ON_BLACK,
    },
    TextBackgroundPreset {
        id: TextBackgroundId::GreenOnBlack,
        label: "Green on black",
        pair: pair([0x00, 0xff, 0x00], [0x00, 0x00, 0x00]),
    },
    TextBackgroundPreset {
        id: TextBackgroundId::WhiteOnBlack,
        label: "White on black",
        pair: pair([0xff, 0xff, 0xff], [0x00, 0x00, 0x00]),
    },
    TextBackgroundPreset {
        id: TextBackgroundId::GnomeLight,
        label: "GNOME light",
        pair: pair([0x17, 0x14, 0x21], [0xff, 0xff, 0xff]),
    },
    TextBackgroundPreset {
        id: TextBackgroundId::GnomeDark,
        label: "GNOME dark",
        pair: pair([0xd0, 0xcf, 0xcc], [0x17, 0x14, 0x21]),
    },
    TextBackgroundPreset {
        id: TextBackgroundId::TangoLight,
        label: "Tango light",
        pair: pair([0x2e, 0x34, 0x36], [0xee, 0xee, 0xec]),
    },
    TextBackgroundPreset {
        id: TextBackgroundId::TangoDark,
        label: "Tango dark",
        pair: pair([0xd3, 0xd7, 0xcf], [0x2e, 0x34, 0x36]),
    },
    TextBackgroundPreset {
        id: TextBackgroundId::SolarizedLight,
        label: "Solarized light",
        pair: pair([0x65, 0x7b, 0x83], [0xfd, 0xf6, 0xe3]),
    },
    TextBackgroundPreset {
        id: TextBackgroundId::SolarizedDark,
        label: "Solarized dark",
        pair: pair([0x83, 0x94, 0x96], [0x00, 0x2b, 0x36]),
    },
];

/// One built-in ANSI palette.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnsiPalette {
    pub id: AnsiPaletteId,
    pub label: &'static str,
    pub colours: [Rgba; 16],
}

/// The six built-in ANSI palettes in upstream order.
///
/// The inherited `GnomeTerminalDefault` source row and upstream's custom editing
/// state are deliberately absent.
pub const ANSI_PALETTE_CATALOGUE: [AnsiPalette; 6] = [
    AnsiPalette {
        id: AnsiPaletteId::Gnome,
        label: "GNOME",
        colours: [
            rgb(0x17, 0x14, 0x21),
            rgb(0xc0, 0x1c, 0x28),
            rgb(0x26, 0xa2, 0x69),
            rgb(0xa2, 0x73, 0x4c),
            rgb(0x12, 0x48, 0x8b),
            rgb(0xa3, 0x47, 0xba),
            rgb(0x2a, 0xa1, 0xb3),
            rgb(0xd0, 0xcf, 0xcc),
            rgb(0x5e, 0x5c, 0x64),
            rgb(0xf6, 0x61, 0x51),
            rgb(0x33, 0xd1, 0x7a),
            rgb(0xe9, 0xad, 0x0c),
            rgb(0x2a, 0x7b, 0xde),
            rgb(0xc0, 0x61, 0xcb),
            rgb(0x33, 0xc7, 0xde),
            rgb(0xff, 0xff, 0xff),
        ],
    },
    AnsiPalette {
        id: AnsiPaletteId::Tango,
        label: "Tango",
        colours: [
            rgb(0x2e, 0x34, 0x36),
            rgb(0xcc, 0x00, 0x00),
            rgb(0x4e, 0x9a, 0x06),
            rgb(0xc4, 0xa0, 0x00),
            rgb(0x34, 0x65, 0xa4),
            rgb(0x75, 0x50, 0x7b),
            rgb(0x06, 0x98, 0x9a),
            rgb(0xd3, 0xd7, 0xcf),
            rgb(0x55, 0x57, 0x53),
            rgb(0xef, 0x29, 0x29),
            rgb(0x8a, 0xe2, 0x34),
            rgb(0xfc, 0xe9, 0x4f),
            rgb(0x72, 0x9f, 0xcf),
            rgb(0xad, 0x7f, 0xa8),
            rgb(0x34, 0xe2, 0xe2),
            rgb(0xee, 0xee, 0xec),
        ],
    },
    AnsiPalette {
        id: AnsiPaletteId::LinuxConsole,
        label: "Linux console",
        colours: [
            rgb(0x00, 0x00, 0x00),
            rgb(0xaa, 0x00, 0x00),
            rgb(0x00, 0xaa, 0x00),
            rgb(0xaa, 0x55, 0x00),
            rgb(0x00, 0x00, 0xaa),
            rgb(0xaa, 0x00, 0xaa),
            rgb(0x00, 0xaa, 0xaa),
            rgb(0xaa, 0xaa, 0xaa),
            rgb(0x55, 0x55, 0x55),
            rgb(0xff, 0x55, 0x55),
            rgb(0x55, 0xff, 0x55),
            rgb(0xff, 0xff, 0x55),
            rgb(0x55, 0x55, 0xff),
            rgb(0xff, 0x55, 0xff),
            rgb(0x55, 0xff, 0xff),
            rgb(0xff, 0xff, 0xff),
        ],
    },
    AnsiPalette {
        id: AnsiPaletteId::Xterm,
        label: "XTerm",
        colours: [
            rgb(0x00, 0x00, 0x00),
            rgb(0xcd, 0x00, 0x00),
            rgb(0x00, 0xcd, 0x00),
            rgb(0xcd, 0xcd, 0x00),
            rgb(0x00, 0x00, 0xee),
            rgb(0xcd, 0x00, 0xcd),
            rgb(0x00, 0xcd, 0xcd),
            rgb(0xe5, 0xe5, 0xe5),
            rgb(0x7f, 0x7f, 0x7f),
            rgb(0xff, 0x00, 0x00),
            rgb(0x00, 0xff, 0x00),
            rgb(0xff, 0xff, 0x00),
            rgb(0x5c, 0x5c, 0xff),
            rgb(0xff, 0x00, 0xff),
            rgb(0x00, 0xff, 0xff),
            rgb(0xff, 0xff, 0xff),
        ],
    },
    AnsiPalette {
        id: AnsiPaletteId::Rxvt,
        label: "Rxvt",
        colours: [
            rgb(0x00, 0x00, 0x00),
            rgb(0xcd, 0x00, 0x00),
            rgb(0x00, 0xcd, 0x00),
            rgb(0xcd, 0xcd, 0x00),
            rgb(0x00, 0x00, 0xcd),
            rgb(0xcd, 0x00, 0xcd),
            rgb(0x00, 0xcd, 0xcd),
            rgb(0xfa, 0xeb, 0xd7),
            rgb(0x40, 0x40, 0x40),
            rgb(0xff, 0x00, 0x00),
            rgb(0x00, 0xff, 0x00),
            rgb(0xff, 0xff, 0x00),
            rgb(0x00, 0x00, 0xff),
            rgb(0xff, 0x00, 0xff),
            rgb(0x00, 0xff, 0xff),
            rgb(0xff, 0xff, 0xff),
        ],
    },
    AnsiPalette {
        id: AnsiPaletteId::Solarized,
        label: "Solarized",
        colours: [
            rgb(0x07, 0x36, 0x42),
            rgb(0xdc, 0x32, 0x2f),
            rgb(0x85, 0x99, 0x00),
            rgb(0xb5, 0x89, 0x00),
            rgb(0x26, 0x8b, 0xd2),
            rgb(0xd3, 0x36, 0x82),
            rgb(0x2a, 0xa1, 0x98),
            rgb(0xee, 0xe8, 0xd5),
            rgb(0x00, 0x2b, 0x36),
            rgb(0xcb, 0x4b, 0x16),
            rgb(0x58, 0x6e, 0x75),
            rgb(0x65, 0x7b, 0x83),
            rgb(0x83, 0x94, 0x96),
            rgb(0x6c, 0x71, 0xc4),
            rgb(0x93, 0xa1, 0xa1),
            rgb(0xfd, 0xf6, 0xe3),
        ],
    },
];

/// Look up one built-in text/background scheme.
///
/// The inherited source row returns `None` because it is not a catalogue entry.
pub fn text_background_preset(id: TextBackgroundId) -> Option<&'static TextBackgroundPreset> {
    TEXT_BACKGROUND_CATALOGUE
        .iter()
        .find(|preset| preset.id == id)
}

/// Look up one built-in ANSI palette.
///
/// The inherited source row returns `None` because it is not a catalogue entry.
pub fn ansi_palette_preset(id: AnsiPaletteId) -> Option<&'static AnsiPalette> {
    ANSI_PALETTE_CATALOGUE.iter().find(|preset| preset.id == id)
}

/// Among's explicit fallback pair, available without consulting a source.
pub fn gray_on_black() -> TextBackgroundPair {
    GRAY_ON_BLACK
}

/// Find the built-in scheme whose pair matches exactly.
///
/// There is deliberately no nearest-colour variant: arbitrary source values are
/// custom, never relabelled as the closest preset.
#[cfg(test)]
pub fn exact_text_background_preset(
    pair: TextBackgroundPair,
) -> Option<&'static TextBackgroundPreset> {
    TEXT_BACKGROUND_CATALOGUE
        .iter()
        .find(|preset| preset.pair == pair)
}

/// Find the built-in palette whose sixteen entries match exactly.
///
/// There is deliberately no nearest-colour variant: arbitrary source values are
/// custom, never relabelled as the closest preset.
#[cfg(test)]
pub fn exact_ansi_palette(colours: &[Rgba; 16]) -> Option<&'static AnsiPalette> {
    ANSI_PALETTE_CATALOGUE
        .iter()
        .find(|preset| preset.colours == *colours)
}

/// Why one inherited dimension could not be resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFailure {
    MissingSchema,
    MissingDefault,
    InvalidIdentity,
    Unreadable,
    ThemeDerived,
    InvalidColour,
}

impl SourceFailure {
    /// A concise explanation suitable for the Terminal page.
    pub fn description(self) -> &'static str {
        match self {
            Self::MissingSchema => "GNOME Terminal colour settings are unavailable",
            Self::MissingDefault => "GNOME Terminal has no default profile",
            Self::InvalidIdentity => "The GNOME Terminal default profile identity is invalid",
            Self::Unreadable => "The GNOME Terminal default profile colours could not be read",
            Self::ThemeDerived => "GNOME Terminal derives its colours from its theme",
            Self::InvalidColour => "The GNOME Terminal profile contains an invalid colour",
        }
    }
}

impl fmt::Display for SourceFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.description())
    }
}

/// One startup read of the GNOME Terminal default profile.
///
/// The two dimensions are independent so a valid palette survives an unusable
/// text/background pair and the other way round.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SourceSnapshot {
    pub(crate) text_background: Result<TextBackgroundPair, SourceFailure>,
    pub(crate) palette: Result<[Rgba; 16], SourceFailure>,
}

impl SourceSnapshot {
    #[cfg(test)]
    pub fn new(
        text_background: Result<TextBackgroundPair, SourceFailure>,
        palette: Result<[Rgba; 16], SourceFailure>,
    ) -> Self {
        Self {
            text_background,
            palette,
        }
    }

    /// A snapshot with both dimensions unavailable for the same reason.
    #[cfg(test)]
    pub fn unavailable(failure: SourceFailure) -> Self {
        Self::new(Err(failure), Err(failure))
    }

    pub fn text_background(&self) -> Result<TextBackgroundPair, SourceFailure> {
        self.text_background
    }

    pub fn ansi_palette(&self) -> Result<[Rgba; 16], SourceFailure> {
        self.palette
    }
}

/// Stored choices. Missing document fields take each enum's default source row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StoredAppearance {
    pub text_background: TextBackgroundId,
    pub ansi_palette: AnsiPaletteId,
}

/// The resolved text/background dimension.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TextBackgroundResolution {
    /// A known pair, either explicit or inherited exactly.
    Exact(TextBackgroundPair),
    /// Among's explicit Gray on black fallback.
    GrayOnBlackFallback { reason: SourceFailure },
}

/// The resolved ANSI dimension.
#[derive(Debug, Clone, PartialEq)]
pub enum PaletteResolution {
    /// A known sixteen-entry palette.
    Exact(Box<[Rgba; 16]>),
    /// VTE's compiled-in palette, restored when inherited data is unavailable.
    VteCompiledDefaults { reason: SourceFailure },
}

/// An effective appearance together with the stored identifiers that produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedAppearance {
    pub text_background: TextBackgroundResolution,
    pub palette: PaletteResolution,
    pub text_background_source: TextBackgroundId,
    pub palette_source: AnsiPaletteId,
}

impl ResolvedAppearance {
    pub fn text_background_id(&self) -> TextBackgroundId {
        self.text_background_source
    }

    pub fn ansi_palette_id(&self) -> AnsiPaletteId {
        self.palette_source
    }

    /// The pair to pass to VTE. It is always known, including the fallback.
    pub fn effective_pair(&self) -> TextBackgroundPair {
        match self.text_background {
            TextBackgroundResolution::Exact(pair) => pair,
            TextBackgroundResolution::GrayOnBlackFallback { .. } => gray_on_black(),
        }
    }
}

/// Resolve stored choices against one startup snapshot.
///
/// An explicit built-in always wins in its own dimension. The stored identifiers
/// remain unchanged, so an unavailable inherited dimension still points at
/// `GnomeTerminalDefault` for the next start.
pub fn resolve(stored: StoredAppearance, source: &SourceSnapshot) -> ResolvedAppearance {
    let text_background = match text_background_preset(stored.text_background) {
        Some(preset) => TextBackgroundResolution::Exact(preset.pair),
        None => match source.text_background() {
            Ok(pair) => TextBackgroundResolution::Exact(pair),
            Err(reason) => TextBackgroundResolution::GrayOnBlackFallback { reason },
        },
    };
    let palette = match ansi_palette_preset(stored.ansi_palette) {
        Some(preset) => PaletteResolution::Exact(Box::new(preset.colours)),
        None => match source.ansi_palette() {
            Ok(colours) => PaletteResolution::Exact(Box::new(colours)),
            Err(reason) => PaletteResolution::VteCompiledDefaults { reason },
        },
    };
    ResolvedAppearance {
        text_background,
        palette,
        text_background_source: stored.text_background,
        palette_source: stored.ansi_palette,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const FIXTURE: &str = include_str!("../tests/fixtures/gnome-terminal-3.56.2-colours.json");

    const TEXT_BACKGROUND_IDS: [TextBackgroundId; 12] = [
        TextBackgroundId::GnomeTerminalDefault,
        TextBackgroundId::BlackOnLightYellow,
        TextBackgroundId::BlackOnWhite,
        TextBackgroundId::GrayOnBlack,
        TextBackgroundId::GreenOnBlack,
        TextBackgroundId::WhiteOnBlack,
        TextBackgroundId::GnomeLight,
        TextBackgroundId::GnomeDark,
        TextBackgroundId::TangoLight,
        TextBackgroundId::TangoDark,
        TextBackgroundId::SolarizedLight,
        TextBackgroundId::SolarizedDark,
    ];

    const ANSI_PALETTE_IDS: [AnsiPaletteId; 7] = [
        AnsiPaletteId::GnomeTerminalDefault,
        AnsiPaletteId::Gnome,
        AnsiPaletteId::Tango,
        AnsiPaletteId::LinuxConsole,
        AnsiPaletteId::Xterm,
        AnsiPaletteId::Rxvt,
        AnsiPaletteId::Solarized,
    ];

    const SOURCE_FAILURES: [SourceFailure; 6] = [
        SourceFailure::MissingSchema,
        SourceFailure::MissingDefault,
        SourceFailure::InvalidIdentity,
        SourceFailure::Unreadable,
        SourceFailure::ThemeDerived,
        SourceFailure::InvalidColour,
    ];

    fn fixture() -> Value {
        serde_json::from_str(FIXTURE).expect("the retained catalogue fixture is valid JSON")
    }

    fn rgb_from(value: &Value) -> Rgba {
        let channels = value.as_array().expect("an RGB array");
        assert_eq!(channels.len(), 3, "an RGB triple");
        let channel =
            |index: usize| channels[index].as_u64().expect("a byte channel") as f32 / 255.0;
        Rgba::new(channel(0), channel(1), channel(2), 1.0).expect("a valid catalogue colour")
    }

    fn pair_from(value: &Value) -> TextBackgroundPair {
        TextBackgroundPair {
            foreground: rgb_from(&value["foreground"]),
            background: rgb_from(&value["background"]),
        }
    }

    fn palette_from(value: &Value) -> [Rgba; 16] {
        let entries = value.as_array().expect("a palette array");
        assert_eq!(entries.len(), 16, "sixteen palette entries");
        std::array::from_fn(|index| rgb_from(&entries[index]))
    }

    fn custom_pair() -> TextBackgroundPair {
        TextBackgroundPair {
            foreground: Rgba::new(0.123_456_7, 0.234_567_8, 0.345_678_9, 0.75)
                .expect("a valid custom foreground"),
            background: Rgba::new(0.987_654_3, 0.876_543_2, 0.765_432_1, 0.5)
                .expect("a valid custom background"),
        }
    }

    fn custom_palette() -> [Rgba; 16] {
        std::array::from_fn(|index| {
            let step = index as f32;
            Rgba::new(step / 37.0, 1.0 - step / 41.0, 0.333_333_3, 0.625)
                .expect("a valid custom palette colour")
        })
    }

    #[test]
    fn the_catalogue_matches_the_pinned_upstream_fixture() {
        let document = fixture();
        assert_eq!(document["source"]["version"], "3.56.2");
        assert_eq!(
            document["source"]["commit"],
            "941d51d151c0e25b285ffe98ad4a38b4ce5dda69"
        );

        let schemes = document["text_background"]
            .as_array()
            .expect("a text/background list");
        assert_eq!(schemes.len(), TEXT_BACKGROUND_CATALOGUE.len());
        for (preset, entry) in TEXT_BACKGROUND_CATALOGUE.iter().zip(schemes) {
            assert_eq!(preset.id.as_str(), entry["id"].as_str().expect("an ID"));
            assert_eq!(preset.label, entry["label"].as_str().expect("a label"));
            assert_eq!(preset.pair, pair_from(entry));
            assert_ne!(preset.id, TextBackgroundId::GnomeTerminalDefault);
        }

        let palettes = document["ansi_palettes"]
            .as_array()
            .expect("a palette list");
        assert_eq!(palettes.len(), ANSI_PALETTE_CATALOGUE.len());
        for (preset, entry) in ANSI_PALETTE_CATALOGUE.iter().zip(palettes) {
            assert_eq!(preset.id.as_str(), entry["id"].as_str().expect("an ID"));
            assert_eq!(preset.label, entry["label"].as_str().expect("a label"));
            assert_eq!(preset.colours, palette_from(&entry["colours"]));
            assert_ne!(preset.id, AnsiPaletteId::GnomeTerminalDefault);
        }

        let gray = text_background_preset(TextBackgroundId::GrayOnBlack).expect("Gray on black");
        assert_eq!(gray.pair, gray_on_black());
    }

    #[test]
    fn every_canonical_colour_is_opaque() {
        for preset in TEXT_BACKGROUND_CATALOGUE {
            assert_eq!(preset.pair.foreground.alpha(), 1.0);
            assert_eq!(preset.pair.background.alpha(), 1.0);
        }
        for preset in ANSI_PALETTE_CATALOGUE {
            for colour in preset.colours {
                assert_eq!(colour.alpha(), 1.0);
            }
        }
    }

    #[test]
    fn the_source_row_is_not_a_builtin_preset() {
        assert_eq!(
            TextBackgroundId::default(),
            TextBackgroundId::GnomeTerminalDefault
        );
        assert_eq!(
            AnsiPaletteId::default(),
            AnsiPaletteId::GnomeTerminalDefault
        );
        assert_eq!(
            TextBackgroundId::default().as_str(),
            "gnome-terminal-default"
        );
        assert_eq!(AnsiPaletteId::default().as_str(), "gnome-terminal-default");
        assert!(text_background_preset(TextBackgroundId::GnomeTerminalDefault).is_none());
        assert!(ansi_palette_preset(AnsiPaletteId::GnomeTerminalDefault).is_none());

        let stored = StoredAppearance::default();
        assert_eq!(
            stored.text_background,
            TextBackgroundId::GnomeTerminalDefault
        );
        assert_eq!(stored.ansi_palette, AnsiPaletteId::GnomeTerminalDefault);
    }

    #[test]
    fn every_known_identifier_round_trips() {
        for id in TEXT_BACKGROUND_IDS {
            assert_eq!(id.as_str().parse(), Ok(id));
        }
        for id in ANSI_PALETTE_IDS {
            assert_eq!(id.as_str().parse(), Ok(id));
        }
    }

    #[test]
    fn an_unknown_identifier_is_an_error_not_the_default() {
        assert_eq!("tango".parse::<TextBackgroundId>(), Err(UnknownPreset));
        assert_eq!("unknown".parse::<TextBackgroundId>(), Err(UnknownPreset));
        assert_eq!("".parse::<TextBackgroundId>(), Err(UnknownPreset));
        assert_eq!("gnome-light".parse::<AnsiPaletteId>(), Err(UnknownPreset));
        assert_eq!("unknown".parse::<AnsiPaletteId>(), Err(UnknownPreset));
        assert_eq!("".parse::<AnsiPaletteId>(), Err(UnknownPreset));
    }

    #[test]
    fn colour_constructors_reject_unusable_channels() {
        assert!(Rgba::new(f32::NAN, 0.0, 0.0, 1.0).is_err());
        assert!(Rgba::new(0.0, f32::NAN, 0.0, 1.0).is_err());
        assert!(Rgba::new(0.0, 0.0, f32::NAN, 1.0).is_err());
        assert!(Rgba::new(0.0, 0.0, 0.0, f32::NAN).is_err());
        assert!(Rgba::new(f32::INFINITY, 0.0, 0.0, 1.0).is_err());
        assert!(Rgba::new(0.0, f32::NEG_INFINITY, 0.0, 1.0).is_err());
        assert!(Rgba::new(-0.001, 0.0, 0.0, 1.0).is_err());
        assert!(Rgba::new(1.001, 0.0, 0.0, 1.0).is_err());
        assert!(Rgba::new(0.0, 0.0, 0.0, 1.001).is_err());
        assert!(Rgba::opaque(f32::INFINITY, 0.0, 0.0).is_err());

        let colour = Rgba::new(0.0, 0.25, 1.0, 0.5).expect("boundary channels are valid");
        assert_eq!(colour.red(), 0.0);
        assert_eq!(colour.green(), 0.25);
        assert_eq!(colour.blue(), 1.0);
        assert_eq!(colour.alpha(), 0.5);

        let opaque = Rgba::opaque(0.0, 0.5, 1.0).expect("an opaque colour is valid");
        assert_eq!(opaque.alpha(), 1.0);
    }

    #[test]
    fn every_source_failure_produces_an_honest_fallback() {
        for failure in SOURCE_FAILURES {
            let resolved = resolve(
                StoredAppearance::default(),
                &SourceSnapshot::unavailable(failure),
            );
            assert_eq!(
                resolved.text_background_source,
                TextBackgroundId::GnomeTerminalDefault
            );
            assert_eq!(resolved.palette_source, AnsiPaletteId::GnomeTerminalDefault);
            assert_eq!(resolved.effective_pair(), gray_on_black());
            assert_eq!(
                resolved.text_background,
                TextBackgroundResolution::GrayOnBlackFallback { reason: failure }
            );
            assert_eq!(
                resolved.palette,
                PaletteResolution::VteCompiledDefaults { reason: failure }
            );
        }
    }

    #[test]
    fn a_valid_source_dimension_survives_the_other_dimension_failing() {
        let pair = custom_pair();
        let palette = custom_palette();

        let text_ok = resolve(
            StoredAppearance::default(),
            &SourceSnapshot::new(Ok(pair), Err(SourceFailure::InvalidColour)),
        );
        assert_eq!(text_ok.effective_pair(), pair);
        assert_eq!(
            text_ok.text_background,
            TextBackgroundResolution::Exact(pair)
        );
        assert_eq!(
            text_ok.palette,
            PaletteResolution::VteCompiledDefaults {
                reason: SourceFailure::InvalidColour
            }
        );

        let palette_ok = resolve(
            StoredAppearance::default(),
            &SourceSnapshot::new(Err(SourceFailure::ThemeDerived), Ok(palette)),
        );
        assert_eq!(palette_ok.effective_pair(), gray_on_black());
        assert_eq!(
            palette_ok.text_background,
            TextBackgroundResolution::GrayOnBlackFallback {
                reason: SourceFailure::ThemeDerived
            }
        );
        assert_eq!(
            palette_ok.palette,
            PaletteResolution::Exact(Box::new(palette))
        );
    }

    #[test]
    fn inherited_custom_values_are_preserved_without_quantization() {
        let pair = custom_pair();
        let palette = custom_palette();
        let resolved = resolve(
            StoredAppearance::default(),
            &SourceSnapshot::new(Ok(pair), Ok(palette)),
        );
        assert_eq!(resolved.effective_pair(), pair);
        assert_eq!(
            resolved.text_background,
            TextBackgroundResolution::Exact(pair)
        );
        assert_eq!(
            resolved.palette,
            PaletteResolution::Exact(Box::new(palette))
        );
        assert_eq!(
            resolved.text_background_source,
            TextBackgroundId::GnomeTerminalDefault
        );
        assert_eq!(resolved.palette_source, AnsiPaletteId::GnomeTerminalDefault);
        assert!(exact_text_background_preset(pair).is_none());
        assert!(exact_ansi_palette(&palette).is_none());
    }

    #[test]
    fn explicit_choices_win_independently_of_the_source() {
        let stored = StoredAppearance {
            text_background: TextBackgroundId::TangoDark,
            ansi_palette: AnsiPaletteId::LinuxConsole,
        };
        let source = SourceSnapshot::unavailable(SourceFailure::MissingDefault);
        let resolved = resolve(stored, &source);
        let tango_dark = text_background_preset(stored.text_background).expect("Tango dark");
        let linux = ansi_palette_preset(stored.ansi_palette).expect("Linux console");
        assert_eq!(resolved.effective_pair(), tango_dark.pair);
        assert_eq!(
            resolved.text_background,
            TextBackgroundResolution::Exact(tango_dark.pair)
        );
        assert_eq!(
            resolved.palette,
            PaletteResolution::Exact(Box::new(linux.colours))
        );
        assert_eq!(resolved.text_background_source, stored.text_background);
        assert_eq!(resolved.palette_source, stored.ansi_palette);
    }

    #[test]
    fn an_explicit_dimension_keeps_working_when_the_other_source_fails() {
        let stored = StoredAppearance {
            text_background: TextBackgroundId::TangoLight,
            ansi_palette: AnsiPaletteId::GnomeTerminalDefault,
        };
        let resolved = resolve(
            stored,
            &SourceSnapshot::new(Ok(custom_pair()), Err(SourceFailure::ThemeDerived)),
        );
        let tango_light = text_background_preset(stored.text_background).expect("Tango light");
        assert_eq!(resolved.effective_pair(), tango_light.pair);
        assert_eq!(
            resolved.text_background,
            TextBackgroundResolution::Exact(tango_light.pair)
        );
        assert_eq!(
            resolved.palette,
            PaletteResolution::VteCompiledDefaults {
                reason: SourceFailure::ThemeDerived
            }
        );
    }

    #[test]
    fn exact_match_helpers_never_choose_a_neighbour() {
        for preset in TEXT_BACKGROUND_CATALOGUE {
            let matched = exact_text_background_preset(preset.pair).expect("an exact scheme");
            assert_eq!(matched.id, preset.id);
        }
        for preset in ANSI_PALETTE_CATALOGUE {
            let matched = exact_ansi_palette(&preset.colours).expect("an exact palette");
            assert_eq!(matched.id, preset.id);
        }

        let mut shifted_palette = ANSI_PALETTE_CATALOGUE[0].colours;
        shifted_palette[0] = Rgba::new(0.0, 0.0, 0.0, 1.0).expect("a valid colour");
        assert!(exact_ansi_palette(&shifted_palette).is_none());

        let mut shifted_pair = TEXT_BACKGROUND_CATALOGUE[0].pair;
        shifted_pair.foreground = Rgba::new(1.0 / 255.0, 0.0, 0.0, 1.0).expect("a valid colour");
        assert!(exact_text_background_preset(shifted_pair).is_none());
    }

    #[test]
    fn repeated_resolution_returns_the_same_appearance() {
        let stored = StoredAppearance {
            text_background: TextBackgroundId::GnomeTerminalDefault,
            ansi_palette: AnsiPaletteId::Solarized,
        };
        let source = SourceSnapshot::new(Ok(custom_pair()), Ok(custom_palette()));
        assert_eq!(resolve(stored, &source), resolve(stored, &source));
    }
}
