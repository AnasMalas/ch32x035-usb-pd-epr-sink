use pd_sink::{
    ExternalPowerInput, InternalTemperature, Milliamps, Millivolts, PowerIndicator, PowerState, PpsOperatingMode,
    PpsStatus, SourceAlert, SourceStatus, TemperatureStatus,
};

#[test]
fn pps_status_exposes_source_measurements_and_current_limit_mode() {
    // 17.2 V / 20 mV = 860; 2.3 A / 50 mA = 46.
    let status = PpsStatus::from_raw_bytes([0x5c, 0x03, 46, 0b0000_1010]);

    assert_eq!(status.output_voltage(), Some(Millivolts(17_200)));
    assert_eq!(status.output_current(), Some(Milliamps(2_300)));
    assert_eq!(status.temperature(), TemperatureStatus::Normal);
    assert_eq!(status.operating_mode(), PpsOperatingMode::CurrentLimit);
    assert!(status.is_current_limited());

    let unsupported = PpsStatus::from_raw_bytes([0xff, 0xff, 0xff, 0]);
    assert_eq!(unsupported.output_voltage(), None);
    assert_eq!(unsupported.output_current(), None);
}

#[test]
fn general_status_decodes_faults_inputs_and_power_limits() {
    let status =
        SourceStatus::from_raw_bytes([42, 0b0001_0110, 0, 0b0001_1010, 0b0000_0100, 0b0010_0010, 0b0000_1001], true);

    assert_eq!(status.internal_temperature(), InternalTemperature::Celsius(42));
    assert_eq!(status.external_power_input(), ExternalPowerInput::Ac);
    assert!(!status.powered_by_battery());
    assert!(status.powered_by_non_battery());
    assert!(status.overcurrent_event());
    assert!(status.overvoltage_event());
    assert_eq!(status.pps_operating_mode(), Some(PpsOperatingMode::CurrentLimit));
    assert_eq!(status.temperature(), TemperatureStatus::Warning);
    assert!(status.power_limited_by_cable());
    assert!(status.power_limited_by_temperature());
    assert!(status.is_power_limited());
    assert_eq!(status.power_state(), PowerState::S0);
    assert_eq!(status.power_indicator(), PowerIndicator::On);

    let outside_pps = SourceStatus::from_raw_bytes(status.raw_bytes(), false);
    assert_eq!(outside_pps.pps_operating_mode(), None);
    assert!(!outside_pps.is_current_limited());
}

#[test]
fn source_alert_identifies_operating_changes_and_capability_reduction() {
    let alert = SourceAlert::from_raw((1 << 28) | (1 << 26));
    assert!(alert.operating_condition_changed());
    assert!(alert.overcurrent_event());

    let reducing = SourceAlert::from_raw((1 << 31) | 5);
    assert!(reducing.source_reducing_capabilities());
}
