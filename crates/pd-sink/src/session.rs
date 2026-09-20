//! Stable lifecycle observations for a managed USB-PD sink session.

#[cfg(any(
    test,
    feature = "ch32x035c8t6",
    feature = "ch32x035f7p6",
    feature = "ch32x035f8u6",
    feature = "ch32x035g8r6",
    feature = "ch32x035g8u6",
    feature = "ch32x035r8t6"
))]
use usbpd::sink::policy_engine::Error as StackSinkError;

#[cfg(any(
    feature = "ch32x035c8t6",
    feature = "ch32x035f7p6",
    feature = "ch32x035f8u6",
    feature = "ch32x035g8r6",
    feature = "ch32x035g8u6",
    feature = "ch32x035r8t6"
))]
pub(crate) const RESET_RETRY_MS: u32 = 20;
#[cfg(any(
    test,
    feature = "ch32x035c8t6",
    feature = "ch32x035f7p6",
    feature = "ch32x035f8u6",
    feature = "ch32x035g8r6",
    feature = "ch32x035g8u6",
    feature = "ch32x035r8t6"
))]
const DETACH_RETRY_MS: u32 = 20;
#[cfg(any(
    test,
    feature = "ch32x035c8t6",
    feature = "ch32x035f7p6",
    feature = "ch32x035f8u6",
    feature = "ch32x035g8r6",
    feature = "ch32x035g8u6",
    feature = "ch32x035r8t6"
))]
const PROTOCOL_RETRY_MS: u32 = 2_000;
#[cfg(any(
    test,
    feature = "ch32x035c8t6",
    feature = "ch32x035f7p6",
    feature = "ch32x035f8u6",
    feature = "ch32x035g8r6",
    feature = "ch32x035g8u6",
    feature = "ch32x035r8t6"
))]
const UNRESPONSIVE_RETRY_MS: u32 = 10_000;

/// Recoverable reason why the managed sink session is being restarted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SinkSessionRecovery {
    /// The physical Type-C connection or VBUS disappeared.
    Detached,
    /// The PHY repeatedly discarded traffic.
    PhyUnstable,
    /// The partner stopped responding. Recovery waits passively for detach or
    /// a bounded retry deadline instead of repeatedly transmitting.
    PortPartnerUnresponsive,
    /// Wire traffic or protocol sequencing could not be recovered inside the
    /// policy engine.
    Protocol,
}

/// Deterministic local policy failure that cannot improve by retrying the same
/// firmware state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SinkSessionLocalError {
    InvalidEprOperationalPdp,
    InvalidRequestForMode,
    InvalidTransmitMessage(SinkSessionTransmitError),
}

/// Local outgoing-message validation failure, translated into a stable
/// library-owned type so applications do not depend on protocol internals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SinkSessionTransmitError {
    UnchunkedExtendedMessagesNotSupported,
    AvsVoltageAlignmentInvalid,
    ExtendedMessageChunkingRequired,
    MessageUnavailableInRevision { message_type: u8, revision: u8 },
}

/// Terminal result returned by a managed session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SinkSessionTerminalError {
    /// The underlying state machine stopped without an error. This is not an
    /// expected outcome for its continuous run loop.
    UnexpectedStop,
    /// Local configuration/request construction is invalid. The load has
    /// already been inhibited and the unchanged session is not retried.
    LocalPolicy(SinkSessionLocalError),
}

/// Board-visible lifecycle observation from a managed sink session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SinkSessionEvent {
    /// PHY reset failed for a reason other than ordinary absence of CC.
    PhyResetFailed { retry_ms: u32 },
    /// CC orientation was detected and the policy engine is about to wait for
    /// VBUS/source capabilities.
    CcDetected,
    /// A recoverable session ended and standard bounded recovery is starting.
    Recovering { reason: SinkSessionRecovery, retry_ms: u32, wait_for_detach: bool },
    /// The session stopped on a deterministic local failure and will not
    /// retry the unchanged state.
    Terminal(SinkSessionTerminalError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(
    test,
    feature = "ch32x035c8t6",
    feature = "ch32x035f7p6",
    feature = "ch32x035f8u6",
    feature = "ch32x035g8r6",
    feature = "ch32x035g8u6",
    feature = "ch32x035r8t6"
))]
pub(crate) struct SinkSessionRetry {
    pub(crate) reason: SinkSessionRecovery,
    pub(crate) delay_ms: u32,
    pub(crate) wait_for_detach: bool,
}

#[cfg(any(
    test,
    feature = "ch32x035c8t6",
    feature = "ch32x035f7p6",
    feature = "ch32x035f8u6",
    feature = "ch32x035g8r6",
    feature = "ch32x035g8u6",
    feature = "ch32x035r8t6"
))]
pub(crate) fn classify_sink_error(error: StackSinkError) -> Result<SinkSessionRetry, SinkSessionTerminalError> {
    let retry = match error {
        StackSinkError::Detached => SinkSessionRetry {
            reason: SinkSessionRecovery::Detached,
            delay_ms: DETACH_RETRY_MS,
            wait_for_detach: false,
        },
        StackSinkError::PhyUnstable => SinkSessionRetry {
            reason: SinkSessionRecovery::PhyUnstable,
            delay_ms: PROTOCOL_RETRY_MS,
            wait_for_detach: false,
        },
        StackSinkError::PortPartnerUnresponsive => SinkSessionRetry {
            reason: SinkSessionRecovery::PortPartnerUnresponsive,
            delay_ms: UNRESPONSIVE_RETRY_MS,
            wait_for_detach: true,
        },
        StackSinkError::Protocol(_) => SinkSessionRetry {
            reason: SinkSessionRecovery::Protocol,
            delay_ms: PROTOCOL_RETRY_MS,
            wait_for_detach: false,
        },
        StackSinkError::InvalidEprOperationalPdp => {
            return Err(SinkSessionTerminalError::LocalPolicy(SinkSessionLocalError::InvalidEprOperationalPdp));
        }
        StackSinkError::InvalidRequestForMode => {
            return Err(SinkSessionTerminalError::LocalPolicy(SinkSessionLocalError::InvalidRequestForMode));
        }
        StackSinkError::InvalidTransmitMessage(error) => {
            use usbpd::protocol_layer::TxValidationError;

            let error = match error {
                TxValidationError::UnchunkedExtendedMessagesNotSupported => {
                    SinkSessionTransmitError::UnchunkedExtendedMessagesNotSupported
                }
                TxValidationError::AvsVoltageAlignmentInvalid => SinkSessionTransmitError::AvsVoltageAlignmentInvalid,
                TxValidationError::ExtendedMessageChunkingRequired => {
                    SinkSessionTransmitError::ExtendedMessageChunkingRequired
                }
                TxValidationError::MessageUnavailableInRevision { message_type, revision } => {
                    SinkSessionTransmitError::MessageUnavailableInRevision { message_type, revision }
                }
            };
            return Err(SinkSessionTerminalError::LocalPolicy(SinkSessionLocalError::InvalidTransmitMessage(error)));
        }
    };
    Ok(retry)
}

#[cfg(test)]
mod tests {
    use usbpd::protocol_layer::{ProtocolError, TxValidationError};

    use super::*;

    #[test]
    fn recoverable_stack_errors_have_bounded_standard_backoff() {
        let cases = [
            (StackSinkError::Detached, SinkSessionRecovery::Detached, 20, false),
            (StackSinkError::PhyUnstable, SinkSessionRecovery::PhyUnstable, 2_000, false),
            (StackSinkError::PortPartnerUnresponsive, SinkSessionRecovery::PortPartnerUnresponsive, 10_000, true),
            (StackSinkError::Protocol(ProtocolError::UnexpectedMessage), SinkSessionRecovery::Protocol, 2_000, false),
        ];

        for (error, reason, delay_ms, wait_for_detach) in cases {
            assert_eq!(classify_sink_error(error), Ok(SinkSessionRetry { reason, delay_ms, wait_for_detach }));
        }
    }

    #[test]
    fn deterministic_local_errors_are_terminal() {
        let cases = [
            (StackSinkError::InvalidEprOperationalPdp, SinkSessionLocalError::InvalidEprOperationalPdp),
            (StackSinkError::InvalidRequestForMode, SinkSessionLocalError::InvalidRequestForMode),
            (
                StackSinkError::InvalidTransmitMessage(TxValidationError::UnchunkedExtendedMessagesNotSupported),
                SinkSessionLocalError::InvalidTransmitMessage(
                    SinkSessionTransmitError::UnchunkedExtendedMessagesNotSupported,
                ),
            ),
            (
                StackSinkError::InvalidTransmitMessage(TxValidationError::AvsVoltageAlignmentInvalid),
                SinkSessionLocalError::InvalidTransmitMessage(SinkSessionTransmitError::AvsVoltageAlignmentInvalid),
            ),
            (
                StackSinkError::InvalidTransmitMessage(TxValidationError::ExtendedMessageChunkingRequired),
                SinkSessionLocalError::InvalidTransmitMessage(
                    SinkSessionTransmitError::ExtendedMessageChunkingRequired,
                ),
            ),
            (
                StackSinkError::InvalidTransmitMessage(TxValidationError::MessageUnavailableInRevision {
                    message_type: 23,
                    revision: 1,
                }),
                SinkSessionLocalError::InvalidTransmitMessage(SinkSessionTransmitError::MessageUnavailableInRevision {
                    message_type: 23,
                    revision: 1,
                }),
            ),
        ];

        for (error, expected) in cases {
            assert_eq!(classify_sink_error(error), Err(SinkSessionTerminalError::LocalPolicy(expected)));
        }
    }
}
