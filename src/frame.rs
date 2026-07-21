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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_stores_all_fields_unchanged() {
        let regions = vec![DamageRegion { x: 0, y: 0, width: 5, height: 5 }];
        let frame = Frame::new(vec![1, 2, 3, 4], 10, 20, regions);
        assert_eq!(frame.rgba, vec![1, 2, 3, 4]);
        assert_eq!(frame.width, 10);
        assert_eq!(frame.height, 20);
        assert_eq!(frame.damage_regions.len(), 1);
    }
}
