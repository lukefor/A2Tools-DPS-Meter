use serde::{Deserialize, Serialize};

/// What a hit did beyond its damage. The bits of the record's flags byte follow
/// the game's damage plotter (`EHitSubType` shifted down one bit, since back
/// attack has its own angle byte): 0x01 ShieldBlock, 0x02 WeaponBlock (Parry),
/// 0x04 Perfect, 0x08 HardHit (Double), 0x10 IronWall, 0x20 Restoration
/// (Regeneration), 0x40 PerfectBlock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SpecialDamage {
    Back,
    Critical,
    ShieldBlock,
    Parry,
    Perfect,
    Double,
    Frontal,
    IronWall,
    Regeneration,
    PerfectBlock,
}

/// The flags byte of a damage record, as `SpecialDamage` values.
pub fn from_hit_flags(flags: u8) -> Vec<SpecialDamage> {
    [
        (0x01, SpecialDamage::ShieldBlock),
        (0x02, SpecialDamage::Parry),
        (0x04, SpecialDamage::Perfect),
        (0x08, SpecialDamage::Double),
        (0x10, SpecialDamage::IronWall),
        (0x20, SpecialDamage::Regeneration),
        (0x40, SpecialDamage::PerfectBlock),
    ]
    .into_iter()
    .filter(|(bit, _)| flags & bit != 0)
    .map(|(_, s)| s)
    .collect()
}
