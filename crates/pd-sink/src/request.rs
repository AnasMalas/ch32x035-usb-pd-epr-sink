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
pub enum CurrentConfidence {
    Advertised,
    DerivedFromPdoPdp,
    DerivedFromSourceInfo,
    PowerLimitedUpperBound,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    pub object_position: u8,
    pub supply: SupplyKind,
    pub message: RequestMessage,
    pub rdo: u32,
    pub pdo_copy: Option<u32>,
    pub voltage: PlannedVoltage,
    pub operating: PlannedOperating,
    pub capability_mismatch: bool,
}

impl RequestPlan {
    pub fn data_objects(self) -> ([u32; 2], usize) {
        match self.pdo_copy {
            Some(pdo) => ([self.rdo, pdo], 2),
            None => ([self.rdo, 0], 1),
        }
    }

    pub fn operating_current(self) -> Option<Milliamps> {
        match self.operating {
            PlannedOperating::Current { operating, .. } => Some(operating),
        }
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
            let plan_current = plan.operating_current().map_or(0, Milliamps::get);
            let replace = best.as_ref().is_none_or(|(best_score, best_plan)| {
                score < *best_score
                    || (score == *best_score && plan_current > best_plan.operating_current().map_or(0, Milliamps::get))
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
            (SourceSupply::Fixed(fixed), Demand::Maximum) => {
                validate_voltage_limit(fixed.voltage, context.limits)?;
                let current =
                    plan_current(fixed.max_current, None, fixed.voltage, 10, CurrentConfidence::Advertised, context)?;
                Ok(current_request_plan(pdo, message, pdo_copy, PlannedVoltage::Fixed(fixed.voltage), current, common))
            }
            (SourceSupply::Fixed(fixed), Demand::Current(requested)) => {
                validate_voltage_limit(fixed.voltage, context.limits)?;
                let current = plan_current(
                    fixed.max_current,
                    Some(requested),
                    fixed.voltage,
                    10,
                    CurrentConfidence::Advertised,
                    context,
                )?;
                Ok(current_request_plan(pdo, message, pdo_copy, PlannedVoltage::Fixed(fixed.voltage), current, common))
            }
            (SourceSupply::Pps(pps), demand @ (Demand::Maximum | Demand::Adjustable { .. })) => {
                let (voltage, current) = maximum_or_adjustable(demand, pps.max_voltage);
                let encoded = adjustable_voltage(pdo.position, voltage, pps.min_voltage, pps.max_voltage, 20, context)?;
                let (source_limit, confidence) = if pps.power_limited {
                    if let Some(pdp) = context.source_present_pdp {
                        (pps.max_current.min(current_for_power(pdp, encoded)), CurrentConfidence::DerivedFromSourceInfo)
                    } else {
                        (pps.max_current, CurrentConfidence::PowerLimitedUpperBound)
                    }
                } else {
                    (pps.max_current, CurrentConfidence::Advertised)
                };
                let planned = plan_current(source_limit, current, encoded, 50, confidence, context)?;
                Ok(adjustable_request_plan(
                    pdo,
                    message,
                    pdo_copy,
                    PlannedVoltage::Adjustable { requested: voltage, encoded, step_mv: 20 },
                    planned,
                    common,
                ))
            }
            (SourceSupply::SprAvs(avs), demand @ (Demand::Maximum | Demand::Adjustable { .. })) => {
                let (voltage, current) = maximum_or_adjustable(demand, avs.max_voltage);
                let encoded =
                    adjustable_voltage(pdo.position, voltage, avs.min_voltage, avs.max_voltage, 100, context)?;
                let source_limit = avs
                    .max_current_at(encoded)
                    .ok_or(PlanError::VoltageOutsideOffer { position: pdo.position, requested: voltage })?;
                let planned = plan_current(source_limit, current, encoded, 50, CurrentConfidence::Advertised, context)?;
                Ok(adjustable_request_plan(
                    pdo,
                    message,
                    pdo_copy,
                    PlannedVoltage::Adjustable { requested: voltage, encoded, step_mv: 100 },
                    planned,
                    common,
                ))
            }
            (SourceSupply::EprAvs(avs), demand @ (Demand::Maximum | Demand::Adjustable { .. })) => {
                let (voltage, current) = maximum_or_adjustable(demand, avs.max_voltage);
                let encoded =
                    adjustable_voltage(pdo.position, voltage, avs.min_voltage, avs.max_voltage, 100, context)?;
                let source_limit = current_for_power(avs.pdp, encoded).min(PROTOCOL_MAX_CURRENT);
                let planned =
                    plan_current(source_limit, current, encoded, 50, CurrentConfidence::DerivedFromPdoPdp, context)?;
                Ok(adjustable_request_plan(
                    pdo,
                    message,
                    pdo_copy,
                    PlannedVoltage::Adjustable { requested: voltage, encoded, step_mv: 100 },
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
    requested: Option<Milliamps>,
    source_limit: Milliamps,
    operating: Milliamps,
    confidence: CurrentConfidence,
    limited_by: LimitReason,
    mismatch: bool,
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
        requested,
        source_limit,
        operating,
        confidence,
        limited_by,
        mismatch: requested.is_some_and(|value| value > source_limit),
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
    let raw_current = current.operating.0 / 10;
    let mismatch_bit = u32::from(current.mismatch) << 26;
    RequestPlan {
        object_position: pdo.position,
        supply: pdo.kind(),
        message,
        rdo: common | mismatch_bit | (raw_current << 10) | raw_current,
        pdo_copy,
        voltage,
        operating: PlannedOperating::Current {
            requested: current.requested,
            source_limit: current.source_limit,
            operating: current.operating,
            confidence: current.confidence,
            limited_by: current.limited_by,
        },
        capability_mismatch: current.mismatch,
    }
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
    let raw_current = current.operating.0 / 50;
    let mismatch_bit = u32::from(current.mismatch) << 26;
    RequestPlan {
        object_position: pdo.position,
        supply: pdo.kind(),
        message,
        rdo: common | mismatch_bit | (raw_voltage << 9) | raw_current,
        pdo_copy,
        voltage,
        operating: PlannedOperating::Current {
            requested: current.requested,
            source_limit: current.source_limit,
            operating: current.operating,
            confidence: current.confidence,
            limited_by: current.limited_by,
        },
        capability_mismatch: current.mismatch,
    }
}
