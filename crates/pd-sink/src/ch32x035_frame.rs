/// Return whether an ordinary wire message needs immediate TX-to-RX
/// turnaround for its GoodCRC or response.
///
/// Only an exact two-byte GoodCRC skips turnaround because GoodCRC itself is
/// never acknowledged. Unknown or malformed frames take the conservative RX
/// path.
pub(crate) fn needs_rx_turnaround(data: &[u8]) -> bool {
    !(data.len() == 2 && data[0] & 0x1f == 1 && data[1] & 0xf0 == 0)
}

#[cfg(test)]
mod tests {
    use super::needs_rx_turnaround;

    #[test]
    fn only_an_exact_goodcrc_skips_turnaround() {
        assert!(!needs_rx_turnaround(&[0x01, 0x00]));
        assert!(!needs_rx_turnaround(&[0x01, 0x0e]));
        assert!(needs_rx_turnaround(&[0x03, 0x00]));
        assert!(needs_rx_turnaround(&[0x02, 0x10]));
        assert!(needs_rx_turnaround(&[0x01, 0x80]));
        assert!(needs_rx_turnaround(&[0x01]));
        assert!(needs_rx_turnaround(&[0x01, 0x00, 0x00]));
    }
}
