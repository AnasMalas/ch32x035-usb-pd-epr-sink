//! Sink Capabilities Extended Data Block (SKEDB).
//!
//! See USB PD R3.2 Table 6.61. The payload is exactly 24 bytes and therefore
//! fits in one chunked Extended Message frame (26 bytes including its Extended
//! Message Header).

/// Sink supports PPS charging/operation.
pub const SINK_MODE_PPS_SUPPORTED: u8 = 1 << 0;
/// Sink is powered from VBUS.
pub const SINK_MODE_VBUS_POWERED: u8 = 1 << 1;
/// Sink supports AVS operation.
pub const SINK_MODE_AVS_SUPPORTED: u8 = 1 << 5;

/// The fixed-size Sink Capabilities Extended Data Block.
///
/// Keeping the descriptor in wire order makes transmission a single bounded
/// copy on small MCUs. [`Self::new_v1_power_descriptor`] creates the subset
/// needed by a programmable power sink; [`Self::from_bytes`] remains available
/// when assigned identity, compliance, battery, or load-characteristic fields
/// need to be populated later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SinkCapabilitiesExtended([u8; Self::DATA_SIZE]);

impl SinkCapabilitiesExtended {
    /// Exact SKEDB payload length.
    pub const DATA_SIZE: usize = 24;

    /// Construct a version 1.0, VBUS-powered descriptor with zeroed optional
    /// identity and product-compliance fields.
    #[allow(clippy::too_many_arguments)]
    pub const fn new_v1_power_descriptor(
        vendor_id: u16,
        product_id: u16,
        sink_modes: u8,
        spr_minimum_pdp_watts: u8,
        spr_operational_pdp_watts: u8,
        spr_maximum_pdp_watts: u8,
        epr_minimum_pdp_watts: u8,
        epr_operational_pdp_watts: u8,
        epr_maximum_pdp_watts: u8,
    ) -> Self {
        Self([
            vendor_id as u8,
            (vendor_id >> 8) as u8,
            product_id as u8,
            (product_id >> 8) as u8,
            0,
            0,
            0,
            0, // XID
            0, // firmware version
            0, // hardware version
            1, // SKEDB version 1.0
            0, // default 150 mA/us load step
            0,
            0, // sink load characteristics
            0, // compliance
            0, // touch temperature standard
            0, // battery information
            sink_modes,
            spr_minimum_pdp_watts,
            spr_operational_pdp_watts,
            spr_maximum_pdp_watts,
            epr_minimum_pdp_watts,
            epr_operational_pdp_watts,
            epr_maximum_pdp_watts,
        ])
    }

    /// Construct a descriptor whose bytes are already in SKEDB wire order.
    pub const fn from_bytes(bytes: [u8; Self::DATA_SIZE]) -> Self {
        Self(bytes)
    }

    /// Return the complete SKEDB in wire order.
    pub const fn bytes(&self) -> &[u8; Self::DATA_SIZE] {
        &self.0
    }

    /// Serialize the SKEDB into an Extended Message payload.
    pub fn to_bytes(self, payload: &mut [u8]) -> usize {
        payload[..Self::DATA_SIZE].copy_from_slice(&self.0);
        Self::DATA_SIZE
    }
}

impl Default for SinkCapabilitiesExtended {
    fn default() -> Self {
        Self::new_v1_power_descriptor(0, 0, SINK_MODE_VBUS_POWERED, 5, 5, 5, 0, 0, 0)
    }
}
