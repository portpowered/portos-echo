use crate::Shutter;

pub(super) const EV_SW: u8 = 0x05;
const SW_CAMERA_LENS_COVER: u16 = 0x09;
const GPIO_KEYS: &[u8] = b"gpio-keys";

pub(super) fn supported(name: &[u8], bits: &[u8]) -> bool {
    name.split(|byte| *byte == 0).next() == Some(GPIO_KEYS) && cover_bit(bits) == Some(true)
}

pub(super) fn switch(bits: &[u8]) -> Shutter {
    match cover_bit(bits) {
        Some(true) => Shutter::Closed,
        Some(false) => Shutter::Open,
        None => Shutter::Unknown,
    }
}

fn cover_bit(bits: &[u8]) -> Option<bool> {
    let bit = usize::from(SW_CAMERA_LENS_COVER);
    Some(bits.get(bit / 8)? & (1 << (bit % 8)) != 0)
}

pub(super) fn event(kind: u16, code: u16, value: i32) -> Shutter {
    match (kind, code, value) {
        (0, 3, _) => Shutter::Unknown, // SYN_DROPPED
        (kind, SW_CAMERA_LENS_COVER, 1) if kind == u16::from(EV_SW) => Shutter::Closed,
        (kind, SW_CAMERA_LENS_COVER, value) if kind == u16::from(EV_SW) && value != 0 => Shutter::Unknown,
        _ => Shutter::Open,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_gpio_keys_only_with_camera_switch_capability() {
        assert!(supported(b"gpio-keys\0", &[0, 2]));
        assert!(!supported(b"gpio-keys\0", &[0, 0]));
        assert!(!supported(b"gpio-keys\0", &[]));
        assert!(!supported(b"gpio-privacy-state\0", &[0, 2]));
    }

    #[test]
    fn shutter_bitmap_keeps_missing_and_mute_distinct_from_closed() {
        assert_eq!(switch(&[]), Shutter::Unknown);
        assert_eq!(switch(&[0]), Shutter::Unknown);
        assert_eq!(switch(&[0, 0]), Shutter::Open);
        assert_eq!(switch(&[0, 64]), Shutter::Open);
        assert_eq!(switch(&[0, 66]), Shutter::Closed);
    }

    #[test]
    fn observes_close_and_loss_even_if_latest_switch_bitmap_is_open() {
        let sequence = [
            event(EV_SW.into(), SW_CAMERA_LENS_COVER, 1),
            event(EV_SW.into(), SW_CAMERA_LENS_COVER, 0),
            switch(&[0, 0]),
        ];
        assert_eq!(
            sequence.into_iter().find(|state| *state != Shutter::Open),
            Some(Shutter::Closed),
        );
        assert_eq!(event(0, 3, 0), Shutter::Unknown);
        assert_eq!(event(EV_SW.into(), SW_CAMERA_LENS_COVER, 2), Shutter::Unknown);
        assert_eq!(event(EV_SW.into(), 0x0e, 1), Shutter::Open);
        assert_eq!(switch(&[0, 0]), Shutter::Open);
    }
}
