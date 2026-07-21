use crate::controller::UserRequest;
use crate::request::{Demand, Preference};
use crate::units::{Milliamps, Millivolts};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Request(UserRequest),
    Identity,
    Capabilities,
    Plans,
    RequestSourceInfo,
    EnterEpr,
    RequestEprCapabilities,
    ExitEpr,
    Status,
    Help,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandError {
    Empty,
    UnknownCommand,
    MissingArgument,
    UnexpectedArgument,
    InvalidNumber,
    InvalidPreference,
    InvalidDemand,
}

/// Parse the deliberately small ASCII command language used by every debug
/// transport. Values are integer millivolts and milliamps.
pub fn parse_command(line: &str) -> Result<Command, CommandError> {
    let mut words = line.split_ascii_whitespace();
    let command = words.next().ok_or(CommandError::Empty)?;
    let parsed = match command {
        "request" | "voltage" => parse_voltage_request(&mut words)?,
        "pdo" => parse_pdo_request(&mut words)?,
        "device" | "identity" => Command::Identity,
        "caps" | "capabilities" => Command::Capabilities,
        "plans" => Command::Plans,
        "source-info" => Command::RequestSourceInfo,
        "enter-epr" => Command::EnterEpr,
        "epr-caps" => Command::RequestEprCapabilities,
        "exit-epr" => Command::ExitEpr,
        "status" => Command::Status,
        "help" => Command::Help,
        _ => return Err(CommandError::UnknownCommand),
    };
    if words.next().is_some() {
        Err(CommandError::UnexpectedArgument)
    } else {
        Ok(parsed)
    }
}

fn parse_voltage_request<'a>(words: &mut impl Iterator<Item = &'a str>) -> Result<Command, CommandError> {
    let voltage = Millivolts(number(words.next())?);
    let mut current = None;
    let mut preference = Preference::Auto;

    if let Some(word) = words.next() {
        if let Some(parsed) = parse_preference(word) {
            preference = parsed;
        } else if word == "max" {
            if let Some(word) = words.next() {
                preference = parse_preference(word).ok_or(CommandError::InvalidPreference)?;
            }
        } else {
            current = Some(Milliamps(parse_u32(word)?));
            if let Some(word) = words.next() {
                preference = parse_preference(word).ok_or(CommandError::InvalidPreference)?;
            }
        }
    }

    Ok(Command::Request(UserRequest::Voltage { voltage, current, preference }))
}

fn parse_pdo_request<'a>(words: &mut impl Iterator<Item = &'a str>) -> Result<Command, CommandError> {
    let position = number(words.next())?;
    let position = u8::try_from(position).map_err(|_| CommandError::InvalidNumber)?;
    let demand = match words.next() {
        None | Some("max") => Demand::Maximum,
        Some("current") => Demand::Current(Milliamps(number(words.next())?)),
        Some("adjust") => {
            let voltage = Millivolts(number(words.next())?);
            let current = match words.next() {
                None | Some("max") => None,
                Some(value) => Some(Milliamps(parse_u32(value)?)),
            };
            Demand::Adjustable { voltage, current }
        }
        Some(_) => return Err(CommandError::InvalidDemand),
    };

    Ok(Command::Request(UserRequest::Pdo { position, demand }))
}

fn parse_preference(value: &str) -> Option<Preference> {
    match value {
        "auto" => Some(Preference::Auto),
        "fixed" => Some(Preference::Fixed),
        "pps" => Some(Preference::Pps),
        "spr-avs" => Some(Preference::SprAvs),
        "epr-avs" | "avs" => Some(Preference::EprAvs),
        "epr-avs-nonstandard" => Some(Preference::EprAvsNonstandard),
        _ => None,
    }
}

fn number(value: Option<&str>) -> Result<u32, CommandError> {
    parse_u32(value.ok_or(CommandError::MissingArgument)?)
}

fn parse_u32(value: &str) -> Result<u32, CommandError> {
    value.parse().map_err(|_| CommandError::InvalidNumber)
}
