pub struct CgbDmgPalette {
    pub bg: [u16; 4],
    pub obp0: [u16; 4],
    pub obp1: [u16; 4],
}

// Mario Land Built-in CGB Palette (Yellow/Green World, Blue/Red Sprites)
pub const MARIO_LAND_PALETTE: CgbDmgPalette = CgbDmgPalette {
    bg:   [0x7FFF, 0x3FE0, 0x0200, 0x0000], // White, Yellow-Green, Dark Green, Black
    obp0: [0x7FFF, 0x7C00, 0x001F, 0x0000], // White, Red, Blue, Black
    obp1: [0x7FFF, 0x7C00, 0x03E0, 0x0000], // White, Red, Green, Black
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DmgPaletteSet {
    pub bg: [u32; 4],
    pub obp0: [u32; 4],
    pub obp1: [u32; 4],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PaletteTheme {
    Auto,         // Detects title from ROM header
    DmgGreen,     // Original Game Boy LCD "Pea Soup"
    PocketGray,   // Game Boy Pocket Grayscale
    MarioLand,    // Super Mario Land CGB colorization
    Metroid2,     // Metroid II CGB colorization
}

// Original Game Boy LCD (Pea Soup Green)
pub const PALETTE_DMG_GREEN: DmgPaletteSet = DmgPaletteSet {
    bg:   [0x9BBC0F, 0x8BAC0F, 0x306230, 0x0F380F],
    obp0: [0x9BBC0F, 0x8BAC0F, 0x306230, 0x0F380F],
    obp1: [0x9BBC0F, 0x8BAC0F, 0x306230, 0x0F380F],
};

const PALETTE_POCKET_GRAY: DmgPaletteSet = DmgPaletteSet {
    bg:   [0xFFFFFF, 0xAAAAAA, 0x555555, 0x000000],
    obp0: [0xFFFFFF, 0xAAAAAA, 0x555555, 0x000000],
    obp1: [0xFFFFFF, 0xAAAAAA, 0x555555, 0x000000],
};

// Super Mario Land CGB Preset
pub const PALETTE_MARIO_LAND: DmgPaletteSet = DmgPaletteSet {
    bg:   [0xFFFFFF, 0x7BFF00, 0x008400, 0x000000], // Yellow/Green background
    obp0: [0xFFFFFF, 0xFF7B00, 0x0000FF, 0x000000], // Red/Blue Mario & Enemies
    obp1: [0xFFFFFF, 0xFF0000, 0x00FF00, 0x000000], // Red/Green Items
};

pub const PALETTE_KIRBYS_DREAMLAND: DmgPaletteSet = DmgPaletteSet {
    bg:   [0xFFFFFF, 0x7733E7, 0x2C2C96, 0x000000], 
    obp0: [0xF7BEF7, 0xFF0000, 0x0000FF, 0x000000],
    obp1: [0xFFFFFF, 0xFF0000, 0xE78686, 0x000000],
};

pub const PALETTE_MARIO_LAND2: DmgPaletteSet = DmgPaletteSet {
    bg:   [0xFFFFFF, 0x7BFF00, 0x11C600, 0x000000], // Yellow/Green background
    obp0: [0xFFFFFF, 0xDFA677, 0x0000FF, 0x000000], // Red/Blue Mario & Enemies
    obp1: [0xFFFFFF, 0xFF0000, 0xDFA677, 0x000000], // Red/Green Items
};

pub const PALETTE_METROID_II: DmgPaletteSet = DmgPaletteSet {
    bg:   [0xFFFFFF, 0x63A5FF, 0x0000FF, 0x000000], // Yellow/Green background
    obp0: [0xFFFF00, 0xFF0000, 0x630000, 0x7BFF31], // Red/Blue Mario & Enemies
    obp1: [0xFFFFFF, 0x630000, 0x00FF00, 0x000000], // Red/Green Items
};

/*
const PALETTE_METROID_2: DmgPaletteSet = DmgPaletteSet {
    bg:   [0xFFFFFF, 0xFF8400, 0x840000, 0x000000],
    obp0: [0xFFFFFF, 0x00C6FF, 0x0000FF, 0x000000],
    obp1: [0xFFFFFF, 0xFFE700, 0xFF0000, 0x000000],
};
*/

impl PaletteTheme {
    /// Resolves the theme to concrete RGB colors (handling AUTO header detection)
    pub fn to_palette_set(self, rom_title: &str) -> DmgPaletteSet {
        match self {
            PaletteTheme::Auto => match rom_title.trim_end_matches('\0') {
                "SUPER MARIOLAND" => PALETTE_MARIO_LAND,
                "MARIOLAND2" => PALETTE_MARIO_LAND2,
                "KIRBY DREAM LAN" => PALETTE_KIRBYS_DREAMLAND,
                "METROID2"  => PALETTE_METROID_II,
                _           => PALETTE_DMG_GREEN, // Fallback default
            },
            PaletteTheme::DmgGreen   => PALETTE_DMG_GREEN,
            PaletteTheme::PocketGray => PALETTE_POCKET_GRAY,
            PaletteTheme::MarioLand  => PALETTE_MARIO_LAND,
            PaletteTheme::Metroid2   => PALETTE_METROID_II,
        }
    }
}