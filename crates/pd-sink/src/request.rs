use crate::capabilities::{
    AdvertisedPdo, CapabilitiesKind, PdoError, PdoValidity, SourceCapabilities, SourceSupply, SupplyKind,
    EPR_AVS_COMPATIBLE_MAX_VOLTAGE, EPR_AVS_STANDARD_MAX_VOLTAGE,
};
use crate::units::{current_for_power, floor_to, Milliamps, Millivolts, Milliwatts};

const PROTOCOL_MAX_CURRENT: Milliamps = Milliamps(5_000);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortMode {
    Spr,
    Epr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestMessage {
    Request,
    EprRequest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preference {
    Auto,
    Position(u8),
    Fixed,
    Pps,
    SprAvs,
    EprAvs,
    /// Explicitly allow a bounded EPR AVS range outside the standard 15-48 V
    /// range when the Source advertised it.
    EprAvsNonstandard,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Demand {
    Maximum,
    Current(Milliamps),
    Adjustable { voltage: Millivolts, current: Option<Milliamps> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestFlags {
    pub usb_communications_capable: bool,
    pub no_usb_suspend: bool,
    pub unchunked_extended_messages_supported: bool,
    pub epr_capable: bool,
}

impl Default for RequestFlags {
    fn default() -> Self {
        Self {
            usb_communications_capable: false,
            no_usb_suspend: true,
            unchunked_extended_messages_supported: false,
            epr_capable: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SinkLimits {
    pub max_voltage: Option<Millivolts>,
    pub board_max_current: Option<Milliamps>,
    pub cable_max_current: Option<Milliamps>,
    pub max_power: Option<Milliwatts>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RequestContext {
    pub flags: RequestFlags,
    pub limits: SinkLimits,
    /// Port Present PDP obtained from Source_Info, when available.
    pub source_present_pdp: Option<Milliwatts>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CurrentConfidence {
    Advertised,
    DerivedFromPdoPdp,
    DerivedFromSourceInfo,
    PowerLimitedUpperBound,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LimitReason {
    Source,
    Protocol,
    Board,
    Cable,
    SinkPower,
    User,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlannedVoltage {
    Fixed(Millivolts),
    Adjustable { requested: Millivolts, encoded: Millivolts, step_mv: u16 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlannedOperating {
    Current {
        requested: Option<Milliamps>,
        source_limit: Milliamps,
        operating: Milliamps,
        confidence: CurrentConfidence,
        limited_by: LimitReason,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestPlan {
    rdo: u32,
    /// Zero means an ordinary Request. Requestable PDOs are always nonzero,
    /// so an EPR Request can retain its exact PDO without a separate tag.
    pdo_copy: u32,
    /// Zero means that the caller requested the Source maximum. A successful
    /// plan can never contain an explicit zero-current request.
    requested_current_ma: u32,
    encoded_voltage_mv: u16,
    source_limit_ma: u16,
    operating_current_ma: u16,
    /// Zero identifies a fixed-voltage plan; adjustable wire steps fit in u8.
    voltage_step_mv: u8,
    requested_voltage_delta_mv: u8,
    supply: SupplyKind,
    confidence: CurrentConfidence,
    limited_by: LimitReason,
}

impl RequestPlan {
    pub const fn object_position(self) -> u8 {
        (self.rdo >> 28) as u8
    }

    pub const fn supply(self) -> SupplyKind {
        self.supply
    }

    pub const fn message(self) -> RequestMessage {
        if self.pdo_copy == 0 {
            RequestMessage::Request
        } else {
            RequestMessage::EprRequest
        }
    }

    pub const fn rdo(self) -> u32 {
        self.rdo
    }

    pub const fn pdo_copy(self) -> Option<u32> {
        if self.pdo_copy == 0 {
            None
        } else {
            Some(self.pdo_copy)
        }
    }

    pub const fn voltage(self) -> PlannedVoltage {
        let encoded = Millivolts(self.encoded_voltage_mv as u32);
        if self.voltage_step_mv == 0 {
            PlannedVoltage::Fixed(encoded)
        } else {
            PlannedVoltage::Adjustable {
                requested: Millivolts(encoded.0 + self.requested_voltage_delta_mv as u32),
                encoded,
                step_mv: self.voltage_step_mv as u16,
            }
        }
    }

    pub const fn operating(self) -> PlannedOperating {
        PlannedOperating::Current {
            requested: if self.requested_current_ma == 0 { None } else { Some(Milliamps(self.requested_current_ma)) },
            source_limit: Milliamps(self.source_limit_ma as u32),
            operating: Milliamps(self.operating_current_ma as u32),
            confidence: self.confidence,
            limited_by: self.limited_by,
        }
    }

    pub const fn capability_mismatch(self) -> bool {
        self.rdo & (1 << 26) != 0
    }

    pub fn data_objects(self) -> ([u32; 2], usize) {
        match self.pdo_copy() {
            Some(pdo) => ([self.rdo, pdo], 2),
            None => ([self.rdo, 0], 1),
        }
    }

    pub const fn operating_current(self) -> Milliamps {
        Milliamps(self.operating_current_ma as u32)
    }

    /// Voltage that is actually encoded in this request.
    pub const fn encoded_voltage(self) -> Millivolts {
        Millivolts(self.encoded_voltage_mv as u32)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanError {
    PositionUnavailable(u8),
    MalformedPdo { position: u8, error: PdoError },
    UnsupportedPdo(u8),
    InvalidDemand { position: u8, supply: SupplyKind },
    VoltageUnavailable(Millivolts),
    VoltageOutsideOffer { position: u8, requested: Millivolts },
    VoltageAboveSinkLimit { requested: Millivolts, maximum: Millivolts },
    CapabilitiesModeMismatch,
    EprModeRequired(u8),
    ZeroOperatingValue,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RequestPlanner;

impl RequestPlanner {
    pub const fn new() -> Self {
        Self
    }

    pub fn for_voltage(
        &self,
        capabilities: &SourceCapabilities,
        mode: PortMode,
        voltage: Millivolts,
        current: Option<Milliamps>,
        preference: Preference,
        context: RequestContext,
    ) -> Result<RequestPlan, PlanError> {
        validate_voltage_limit(voltage, context.limits)?;

        if let Preference::Position(position) = preference {
            let pdo = capabilities.pdo(position).ok_or(PlanError::PositionUnavailable(position))?;
            let demand = match pdo.supply {
                SourceSupply::Fixed(fixed) if fixed.voltage == voltage => {
                    current.map_or(Demand::Maximum, Demand::Current)
                }
                SourceSupply::Pps(_) | SourceSupply::SprAvs(_) | SourceSupply::EprAvs(_) => {
                    Demand::Adjustable { voltage, current }
                }
                _ => return Err(PlanError::VoltageOutsideOffer { position, requested: voltage }),
            };
            return self.for_pdo(capabilities, mode, position, demand, context);
        }

        let mut best: Option<(u8, RequestPlan)> = None;
        let mut epr_candidate = None;

        for pdo in capabilities.iter().filter(|pdo| pdo.is_requestable()) {
            let Some(score) = voltage_match_score(pdo, voltage, preference) else {
                continue;
            };

            if pdo.requires_epr_mode() && !matches!(mode, PortMode::Epr) {
                epr_candidate.get_or_insert(pdo.position);
                continue;
            }

            let demand = match pdo.supply {
                SourceSupply::Fixed(_) => current.map_or(Demand::Maximum, Demand::Current),
                SourceSupply::Pps(_) | SourceSupply::SprAvs(_) | SourceSupply::EprAvs(_) => {
                    Demand::Adjustable { voltage, current }
                }
                _ => continue,
            };

            let plan = self.for_pdo(capabilities, mode, pdo.position, demand, context)?;
            let plan_current = plan.operating_current().get();
            let replace = best.as_ref().is_none_or(|(best_score, best_plan)| {
                score < *best_score || (score == *best_score && plan_current > best_plan.operating_current().get())
            });
            if replace {
                best = Some((score, plan));
            }
        }

        if let Some((_, plan)) = best {
            Ok(plan)
        } else if let Some(position) = epr_candidate {
            Err(PlanError::EprModeRequired(position))
        } else {
            Err(PlanError::VoltageUnavailable(voltage))
        }
    }

    pub fn for_pdo(
        &self,
        capabilities: &SourceCapabilities,
        mode: PortMode,
        position: u8,
        demand: Demand,
        context: RequestContext,
    ) -> Result<RequestPlan, PlanError> {
        let pdo = capabilities.pdo(position).ok_or(PlanError::PositionUnavailable(position))?;

        match pdo.validity {
            PdoValidity::Valid | PdoValidity::Compatible => {}
            PdoValidity::Malformed(error) => return Err(PlanError::MalformedPdo { position, error }),
            PdoValidity::ZeroPadding => return Err(PlanError::PositionUnavailable(position)),
            PdoValidity::Unsupported => return Err(PlanError::UnsupportedPdo(position)),
        }

        if pdo.requires_epr_mode() && !matches!(mode, PortMode::Epr) {
            return Err(PlanError::EprModeRequired(position));
        }
        match (capabilities.kind(), mode) {
            (CapabilitiesKind::Spr, PortMode::Spr) | (CapabilitiesKind::Epr, PortMode::Epr) => {}
            _ => return Err(PlanError::CapabilitiesModeMismatch),
        }

        let message = if matches!(mode, PortMode::Epr) { RequestMessage::EprRequest } else { RequestMessage::Request };
        let pdo_copy = matches!(message, RequestMessage::EprRequest).then_some(pdo.raw);
        let common = common_rdo_bits(position, context.flags);

        match (pdo.supply, demand) {
            (SourceSupply::Fixed(fixed), demand @ (Demand::Maximum | Demand::Current(_))) => {
                validate_voltage_limit(fixed.voltage, context.limits)?;
                let requested = match demand {
                    Demand::Maximum => None,
                    Demand::Current(requested) => Some(requested),
                    Demand::Adjustable { .. } => unreachable!(),
                };
                let current = plan_current(
                    fixed.max_current,
                    requested,
                    fixed.voltage,
                    10,
                    CurrentConfidence::Advertised,
                    context,
                )?;
                Ok(current_request_plan(pdo, message, pdo_copy, PlannedVoltage::Fixed(fixed.voltage), current, common))
            }
            (
                supply @ (SourceSupply::Pps(_) | SourceSupply::SprAvs(_) | SourceSupply::EprAvs(_)),
                demand @ (Demand::Maximum | Demand::Adjustable { .. }),
            ) => {
                let (minimum, maximum, voltage_step) = match supply {
                    SourceSupply::Pps(pps) => (pps.min_voltage, pps.max_voltage, 20),
                    SourceSupply::SprAvs(avs) => (avs.min_voltage, avs.max_voltage, 100),
                    SourceSupply::EprAvs(avs) => (avs.min_voltage, avs.max_voltage, 100),
                    _ => unreachable!(),
                };
                let (voltage, current) = maximum_or_adjustable(demand, maximum);
                let encoded = adjustable_voltage(pdo.position, voltage, minimum, maximum, voltage_step, context)?;
                let (source_limit, confidence) = match supply {
                    SourceSupply::Pps(pps) if pps.power_limited => match context.source_present_pdp {
                        Some(pdp) => (
                            pps.max_current.min(current_for_power(pdp, encoded)),
                            CurrentConfidence::DerivedFromSourceInfo,
                        ),
                        None => (pps.max_current, CurrentConfidence::PowerLimitedUpperBound),
                    },
                    SourceSupply::Pps(pps) => (pps.max_current, CurrentConfidence::Advertised),
                    SourceSupply::SprAvs(avs) => (
                        avs.max_current_at(encoded)
                            .ok_or(PlanError::VoltageOutsideOffer { position: pdo.position, requested: voltage })?,
                        CurrentConfidence::Advertised,
                    ),
                    SourceSupply::EprAvs(avs) => (
                        current_for_power(avs.pdp, encoded).min(PROTOCOL_MAX_CURRENT),
                        CurrentConfidence::DerivedFromPdoPdp,
                    ),
                    _ => unreachable!(),
                };
                let planned = plan_current(source_limit, current, encoded, 50, confidence, context)?;
                Ok(adjustable_request_plan(
                    pdo,
                    message,
                    pdo_copy,
                    PlannedVoltage::Adjustable { requested: voltage, encoded, step_mv: voltage_step as u16 },
                    planned,
                    common,
                ))
            }
            (supply, _) => Err(PlanError::InvalidDemand { position, supply: supply.kind() }),
        }
    }
}

fn maximum_or_adjustable(demand: Demand, maximum: Millivolts) -> (Millivolts, Option<Milliamps>) {
    match demand {
        Demand::Maximum => (maximum, None),
        Demand::Adjustable { voltage, current } => (voltage, current),
        Demand::Current(_) => unreachable!("caller only passes adjustable demand kinds"),
    }
}

#[derive(Clone, Copy)]
struct CurrentPlan {
    requested_ma: u32,
    source_limit_ma: u16,
    operating_ma: u16,
    confidence: CurrentConfidence,
    limited_by: LimitReason,
}

impl CurrentPlan {
    const fn operating(self) -> Milliamps {
        Milliamps(self.operating_ma as u32)
    }

    const fn mismatch(self) -> bool {
        self.requested_ma > self.source_limit_ma as u32
    }
}

fn voltage_match_score(pdo: AdvertisedPdo, voltage: Millivolts, preference: Preference) -> Option<u8> {
    let allow_nonstandard_epr_avs = matches!(preference, Preference::EprAvsNonstandard);
    let (minimum, maximum) =
        if allow_nonstandard_epr_avs { pdo.voltage_range()? } else { pdo.standard_voltage_range()? };
    if voltage < minimum || voltage > maximum {
        return None;
    }

    let (kind, auto_score) = match pdo.supply {
        SourceSupply::Fixed(fixed) if fixed.voltage == voltage => (Preference::Fixed, 0),
        SourceSupply::Pps(_) => (Preference::Pps, 10),
        SourceSupply::SprAvs(_) => (Preference::SprAvs, 20),
        SourceSupply::EprAvs(_) => (Preference::EprAvs, 30),
        _ => return None,
    };

    match preference {
        Preference::Auto => Some(auto_score),
        Preference::EprAvsNonstandard if matches!(kind, Preference::EprAvs) => Some(0),
        explicit if explicit == kind => Some(0),
        _ => None,
    }
}

fn common_rdo_bits(position: u8, flags: RequestFlags) -> u32 {
    (u32::from(position) << 28)
        | (u32::from(flags.usb_communications_capable) << 25)
        | (u32::from(flags.no_usb_suspend) << 24)
        | (u32::from(flags.unchunked_extended_messages_supported) << 23)
        | (u32::from(flags.epr_capable) << 22)
}

fn validate_voltage_limit(voltage: Millivolts, limits: SinkLimits) -> Result<(), PlanError> {
    let maximum = limits.max_voltage.unwrap_or(EPR_AVS_STANDARD_MAX_VOLTAGE).min(EPR_AVS_COMPATIBLE_MAX_VOLTAGE);
    if voltage > maximum {
        Err(PlanError::VoltageAboveSinkLimit { requested: voltage, maximum })
    } else {
        Ok(())
    }
}

fn adjustable_voltage(
    position: u8,
    requested: Millivolts,
    minimum: Millivolts,
    maximum: Millivolts,
    step: u32,
    context: RequestContext,
) -> Result<Millivolts, PlanError> {
    validate_voltage_limit(requested, context.limits)?;
    if requested < minimum || requested > maximum {
        return Err(PlanError::VoltageOutsideOffer { position, requested });
    }
    let encoded = Millivolts(floor_to(requested.0, step));
    if encoded < minimum || encoded > maximum {
        Err(PlanError::VoltageOutsideOffer { position, requested })
    } else {
        Ok(encoded)
    }
}

fn plan_current(
    advertised_source_limit: Milliamps,
    requested: Option<Milliamps>,
    voltage_for_power_limit: Millivolts,
    step: u32,
    mut confidence: CurrentConfidence,
    context: RequestContext,
) -> Result<CurrentPlan, PlanError> {
    let mut source_limit = Milliamps(floor_to(advertised_source_limit.min(PROTOCOL_MAX_CURRENT).0, step));
    let mut source_info_limited = false;
    if let Some(present_pdp) = context.source_present_pdp {
        let present_limit = Milliamps(floor_to(current_for_power(present_pdp, voltage_for_power_limit).0, step));
        if present_limit < source_limit {
            source_limit = present_limit;
            confidence = CurrentConfidence::DerivedFromSourceInfo;
            source_info_limited = true;
        }
    }
    let mut operating = source_limit;
    let mut limited_by = if source_info_limited || advertised_source_limit <= PROTOCOL_MAX_CURRENT {
        LimitReason::Source
    } else {
        LimitReason::Protocol
    };

    apply_current_limit(&mut operating, &mut limited_by, context.limits.board_max_current, LimitReason::Board);
    apply_current_limit(&mut operating, &mut limited_by, context.limits.cable_max_current, LimitReason::Cable);
    if let Some(max_power) = context.limits.max_power {
        apply_current_limit(
            &mut operating,
            &mut limited_by,
            Some(current_for_power(max_power, voltage_for_power_limit)),
            LimitReason::SinkPower,
        );
    }
    apply_current_limit(&mut operating, &mut limited_by, requested, LimitReason::User);

    operating = Milliamps(floor_to(operating.0, step));
    if operating == Milliamps(0) {
        return Err(PlanError::ZeroOperatingValue);
    }

    Ok(CurrentPlan {
        requested_ma: requested.map_or(0, Milliamps::get),
        source_limit_ma: source_limit.0 as u16,
        operating_ma: operating.0 as u16,
        confidence,
        limited_by,
    })
}

fn apply_current_limit(
    current: &mut Milliamps,
    limited_by: &mut LimitReason,
    candidate: Option<Milliamps>,
    reason: LimitReason,
) {
    if let Some(candidate) = candidate {
        if candidate < *current {
            *current = candidate;
            *limited_by = reason;
        }
    }
}

fn current_request_plan(
    pdo: AdvertisedPdo,
    message: RequestMessage,
    pdo_copy: Option<u32>,
    voltage: PlannedVoltage,
    current: CurrentPlan,
    common: u32,
) -> RequestPlan {
    let raw_current = current.operating().0 / 10;
    let mismatch_bit = u32::from(current.mismatch()) << 26;
    compact_request_plan(
        pdo,
        message,
        pdo_copy,
        voltage,
        current,
        common | mismatch_bit | (raw_current << 10) | raw_current,
    )
}

fn adjustable_request_plan(
    pdo: AdvertisedPdo,
    message: RequestMessage,
    pdo_copy: Option<u32>,
    voltage: PlannedVoltage,
    current: CurrentPlan,
    common: u32,
) -> RequestPlan {
    let PlannedVoltage::Adjustable { encoded, step_mv, .. } = voltage else {
        unreachable!("adjustable request helper requires an adjustable voltage plan")
    };
    let raw_voltage = if step_mv == 20 { encoded.0 / 20 } else { encoded.0 / 25 };
    let raw_current = current.operating().0 / 50;
    let mismatch_bit = u32::from(current.mismatch()) << 26;
    compact_request_plan(
        pdo,
        message,
        pdo_copy,
        voltage,
        current,
        common | mismatch_bit | (raw_voltage << 9) | raw_current,
    )
}

fn compact_request_plan(
    pdo: AdvertisedPdo,
    message: RequestMessage,
    pdo_copy: Option<u32>,
    voltage: PlannedVoltage,
    current: CurrentPlan,
    rdo: u32,
) -> RequestPlan {
    let (requested, encoded, step_mv) = match voltage {
        PlannedVoltage::Fixed(voltage) => (voltage, voltage, 0),
        PlannedVoltage::Adjustable { requested, encoded, step_mv } => (requested, encoded, step_mv),
    };
    let voltage_delta = requested.0 - encoded.0;
    debug_assert!(encoded.0 <= u32::from(u16::MAX));
    debug_assert!(step_mv <= u16::from(u8::MAX));
    debug_assert!(voltage_delta <= u32::from(u8::MAX));
    debug_assert_eq!(matches!(message, RequestMessage::EprRequest), pdo_copy.is_some());
    debug_assert!(pdo_copy != Some(0));

    RequestPlan {
        rdo,
        pdo_copy: pdo_copy.unwrap_or(0),
        requested_current_ma: current.requested_ma,
        encoded_voltage_mv: encoded.0 as u16,
        source_limit_ma: current.source_limit_ma,
        operating_current_ma: current.operating_ma,
        voltage_step_mv: step_mv as u8,
        requested_voltage_delta_mv: voltage_delta as u8,
        supply: pdo.kind(),
        confidence: current.confidence,
        limited_by: current.limited_by,
    }
}
