#[derive(Clone, Copy, PartialEq)]
pub enum GbVariant {
    Dmg,
    Mgb,
    Cgb,
    Sgb,
    Sgb2,
    Agb,
}

pub struct GbCpuConfig {
    pub variant: GbVariant,
    pub initial_a: u8,
    pub initial_f: u8,
    pub initial_bc: u16,
    pub initial_de: u16,
    pub initial_hl: u16,
    pub initial_sp: u16,
    pub initial_pc: u16,             // set to 0x0100 to skip bios
    pub supports_double_speed: bool,
}

impl GbCpuConfig {
    pub fn for_variant(variant: GbVariant) -> Self {
        match variant {
            // Starting values assume skipping the BIOS. Want to load bios as an option in the future.
            GbVariant::Dmg => Self {
                variant, initial_a: 0x01, initial_f: 0xB0,
                initial_bc: 0x0013, initial_de: 0x00D8, initial_hl: 0x014D,
                initial_sp: 0xFFFE, supports_double_speed: false,
                initial_pc: 0x0100,
            },
            GbVariant::Mgb => Self {
                variant, initial_a: 0xFF, initial_f: 0xB0,
                initial_bc: 0x0013, initial_de: 0x00D8, initial_hl: 0x014D,
                initial_sp: 0xFFFE, supports_double_speed: false,
                initial_pc: 0x0100,
            },
            GbVariant::Cgb => Self {
                variant, initial_a: 0x11, initial_f: 0x80,
                initial_bc: 0x0000, initial_de: 0xFF56, initial_hl: 0x000D,
                initial_sp: 0xFFFE, supports_double_speed: true,
                initial_pc: 0x0100,
            },
            GbVariant::Agb => Self {
                variant, initial_a: 0x11, initial_f: 0x80,
                initial_bc: 0x0100, initial_de: 0xFF56, initial_hl: 0x000D,
                initial_sp: 0xFFFE, supports_double_speed: true,
                initial_pc: 0x0100,
            },
            GbVariant::Sgb | GbVariant::Sgb2 => Self {
                variant,
                initial_a: 0x01,  // Identifies as a classic monochrome Game Boy
                initial_f: 0xB0,
                initial_bc: 0x0013,
                initial_de: 0x00D8,
                initial_hl: 0x014D,
                initial_sp: 0xFFFE,
                initial_pc: 0x0100,
                supports_double_speed: false, // Runs strictly at normal speed
            },
        }
    }
}