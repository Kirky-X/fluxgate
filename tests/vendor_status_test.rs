#[cfg(test)]
mod tests {
    use nexusflow::vendor::{VendorState, VendorStatus};

    #[test]
    fn test_vendor_state_new() {
        let state = VendorState::new();
        assert_eq!(state.get_status(), VendorStatus::Available);
        assert_eq!(state.get_error_count(), 0);
    }

    #[test]
    fn test_vendor_state_set_available() {
        let state = VendorState::new();
        state.set_status(VendorStatus::Available);
        assert_eq!(state.get_status(), VendorStatus::Available);
    }

    #[test]
    fn test_vendor_state_set_busy() {
        let state = VendorState::new();
        state.set_status(VendorStatus::Busy);
        assert_eq!(state.get_status(), VendorStatus::Busy);
    }

    #[test]
    fn test_vendor_state_set_throttled() {
        let state = VendorState::new();
        state.set_status(VendorStatus::Throttled);
        assert_eq!(state.get_status(), VendorStatus::Throttled);
    }

    #[test]
    fn test_vendor_state_set_unavailable() {
        let state = VendorState::new();
        state.set_status(VendorStatus::Unavailable);
        assert_eq!(state.get_status(), VendorStatus::Unavailable);
    }

    #[test]
    fn test_vendor_state_inc_error() {
        let state = VendorState::new();
        assert_eq!(state.get_error_count(), 0);

        state.inc_error();
        assert_eq!(state.get_error_count(), 1);

        state.inc_error();
        assert_eq!(state.get_error_count(), 2);
    }

    #[test]
    fn test_vendor_state_reset_error() {
        let state = VendorState::new();

        state.inc_error();
        state.inc_error();
        assert_eq!(state.get_error_count(), 2);

        state.reset_error();
        assert_eq!(state.get_error_count(), 0);
    }

    #[test]
    fn test_vendor_state_mark_busy() {
        let state = VendorState::new();
        state.mark_busy();
        assert_eq!(state.get_status(), VendorStatus::Busy);
    }

    #[test]
    fn test_vendor_state_mark_available() {
        let state = VendorState::new();
        state.mark_busy();
        state.mark_available();
        assert_eq!(state.get_status(), VendorStatus::Available);
    }

    #[test]
    fn test_vendor_state_mark_throttled() {
        let state = VendorState::new();
        let backoff = std::time::Duration::from_secs(60);
        state.mark_throttled(backoff);
        assert_eq!(state.get_status(), VendorStatus::Throttled);
    }

    #[test]
    fn test_vendor_state_mark_unavailable() {
        let state = VendorState::new();
        state.mark_unavailable();
        assert_eq!(state.get_status(), VendorStatus::Unavailable);
    }

    #[test]
    fn test_vendor_state_status_transitions() {
        let state = VendorState::new();

        state.mark_busy();
        assert_eq!(state.get_status(), VendorStatus::Busy);

        let backoff = std::time::Duration::from_secs(30);
        state.mark_throttled(backoff);
        assert_eq!(state.get_status(), VendorStatus::Throttled);

        state.mark_available();
        assert_eq!(state.get_status(), VendorStatus::Available);

        state.mark_unavailable();
        assert_eq!(state.get_status(), VendorStatus::Unavailable);
    }

    #[test]
    fn test_vendor_status_from_u8() {
        assert_eq!(VendorStatus::from_u8(0), VendorStatus::Available);
        assert_eq!(VendorStatus::from_u8(1), VendorStatus::Busy);
        assert_eq!(VendorStatus::from_u8(2), VendorStatus::Throttled);
        assert_eq!(VendorStatus::from_u8(3), VendorStatus::Unavailable);
        assert_eq!(VendorStatus::from_u8(99), VendorStatus::Available);
    }
}
