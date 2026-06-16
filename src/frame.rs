use crate::capture::DamageRegion;

pub struct Frame {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub damage_regions: Vec<DamageRegion>,
}

impl Frame {
    pub fn new(rgba: Vec<u8>, width: u32, height: u32, damage_regions: Vec<DamageRegion>) -> Self {
        Self { rgba, width, height, damage_regions }
    }
}
