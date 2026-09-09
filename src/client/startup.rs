use super::*;

/// Runs the thin client and enters the main event loop.
pub fn run_client(local_startup_error: Option<String>) -> io::Result<()> {
    run_client_with_mode(None, None, "connecting to server", local_startup_error)
}

pub(crate) fn retain_local_startup_result<T>(
    result: io::Result<T>,
    federated: bool,
    diagnostic: &mut Option<String>,
) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if federated => {
            warn!(%error, "Local startup failed; keeping saved machines available");
            diagnostic.get_or_insert_with(|| error.to_string());
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_local_handshake_failure_keeps_its_reason_and_first_diagnostic() {
        let mut diagnostic = None;
        let rejection = ClientError::HandshakeRejected {
            version: 99,
            error: "required input codec unavailable".into(),
        }
        .to_string();
        let connection = retain_local_startup_result::<()>(
            Err(io::Error::other(rejection.clone())),
            true,
            &mut diagnostic,
        )
        .unwrap();
        assert!(connection.is_none());
        assert_eq!(diagnostic.as_deref(), Some(rejection.as_str()));
        assert!(retain_local_startup_result::<()>(
            Err(io::Error::other("later connection failure")),
            true,
            &mut diagnostic
        )
        .unwrap()
        .is_none());
        assert_eq!(diagnostic.as_deref(), Some(rejection.as_str()));
        assert_eq!(
            retain_local_startup_result(Ok(42), true, &mut None).unwrap(),
            Some(42)
        );
    }
}

#[cfg(unix)]
pub fn run_terminal_attach(terminal_id: String, takeover: bool) -> io::Result<()> {
    run_client_with_mode(
        Some((terminal_id, takeover)),
        Some(AttachEscapeState::default()),
        "attaching to terminal",
        None,
    )
}

#[cfg(windows)]
pub fn run_terminal_attach(_terminal_id: String, _takeover: bool) -> io::Result<()> {
    debug_assert!(!crate::platform::capabilities().direct_terminal_attach);
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "direct terminal attach is not supported on Windows yet",
    ))
}
