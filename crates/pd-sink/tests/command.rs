use pd_sink::{parse_command, Command, CommandError, Demand, Milliamps, Millivolts, Preference, UserRequest};

#[test]
fn parses_voltage_requests_for_every_adjustable_family() {
    assert_eq!(
        parse_command("request 19400 3000 pps"),
        Ok(Command::Request(UserRequest::Voltage {
            voltage: Millivolts(19_400),
            current: Some(Milliamps(3_000)),
            preference: Preference::Pps,
        }))
    );
    assert_eq!(
        parse_command("voltage 19400 epr-avs"),
        Ok(Command::Request(UserRequest::Voltage {
            voltage: Millivolts(19_400),
            current: None,
            preference: Preference::EprAvs,
        }))
    );
    assert_eq!(
        parse_command("voltage 10000 epr-avs-nonstandard"),
        Ok(Command::Request(UserRequest::Voltage {
            voltage: Millivolts(10_000),
            current: None,
            preference: Preference::EprAvsNonstandard,
        }))
    );
    assert_eq!(
        parse_command("request 48000 fixed"),
        Ok(Command::Request(UserRequest::Voltage {
            voltage: Millivolts(48_000),
            current: None,
            preference: Preference::Fixed,
        }))
    );
}

#[test]
fn parses_direct_pdo_requests_without_guessing_the_rdo_kind() {
    assert_eq!(
        parse_command("pdo 8 max"),
        Ok(Command::Request(UserRequest::Pdo { position: 8, demand: Demand::Maximum }))
    );
    assert_eq!(
        parse_command("pdo 2 current 1750"),
        Ok(Command::Request(UserRequest::Pdo { position: 2, demand: Demand::Current(Milliamps(1_750)) }))
    );
    assert_eq!(
        parse_command("pdo 9 adjust 19400 2500"),
        Ok(Command::Request(UserRequest::Pdo {
            position: 9,
            demand: Demand::Adjustable { voltage: Millivolts(19_400), current: Some(Milliamps(2_500)) },
        }))
    );
}

#[test]
fn parses_control_and_diagnostic_commands() {
    assert_eq!(parse_command("device"), Ok(Command::Identity));
    assert_eq!(parse_command("identity"), Ok(Command::Identity));
    assert_eq!(parse_command("caps"), Ok(Command::Capabilities));
    assert_eq!(parse_command("plans"), Ok(Command::Plans));
    assert_eq!(parse_command("source-info"), Ok(Command::RequestSourceInfo));
    assert_eq!(parse_command("source-status"), Ok(Command::RequestSourceStatus));
    assert_eq!(parse_command("pd-status"), Ok(Command::RequestSourceStatus));
    assert_eq!(parse_command("pps-status"), Ok(Command::RequestPpsStatus));
    assert_eq!(parse_command("enter-epr"), Ok(Command::EnterEpr));
    assert_eq!(parse_command("epr-caps"), Ok(Command::RequestEprCapabilities));
    assert_eq!(parse_command("exit-epr"), Ok(Command::ExitEpr));
    assert_eq!(parse_command("status"), Ok(Command::Status));
    assert_eq!(parse_command("help"), Ok(Command::Help));
}

#[test]
fn rejects_ambiguous_or_malformed_input() {
    assert_eq!(parse_command(""), Err(CommandError::Empty));
    assert_eq!(parse_command("request nope"), Err(CommandError::InvalidNumber));
    assert_eq!(parse_command("request 19400 3000 mystery"), Err(CommandError::InvalidPreference));
    assert_eq!(parse_command("pdo 2 bananas"), Err(CommandError::InvalidDemand));
    assert_eq!(parse_command("pdo 3 power 30000"), Err(CommandError::InvalidDemand));
    assert_eq!(parse_command("status extra"), Err(CommandError::UnexpectedArgument));
}
