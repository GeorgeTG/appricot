//! Frame pacing by client credits.
//!
//! There is no timer. A frame is sent when a surface has damage and a credit is free, and the
//! client's ack for a frame frees its credit. An idle app costs nothing, and a slow client
//! slows the sender instead of queueing frames without bound. A sender that paces itself by a
//! clock has no way to learn that the far end is behind: it discovers a slow link as a growing
//! queue, which is latency the user feels and memory nobody budgeted. The credit count itself
//! is fixed by the wire spec (`docs/protocol/`).

/// Credits for frames sent and not yet acked by the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameCredits {
    outstanding: u32,
    limit: u32,
}

impl FrameCredits {
    /// Allows up to `limit` frames in flight. A limit of zero is raised to one.
    pub fn new(limit: u32) -> Self {
        Self {
            outstanding: 0,
            limit: limit.max(1),
        }
    }

    /// Takes a credit for one frame. Returns false when none is free: wait for an ack.
    pub fn try_send(&mut self) -> bool {
        if self.outstanding >= self.limit {
            return false;
        }
        self.outstanding += 1;
        true
    }

    /// Frees the credit of one acked frame. An ack with nothing outstanding changes nothing.
    pub fn ack(&mut self) {
        self.outstanding = self.outstanding.saturating_sub(1);
    }

    /// Frames sent and not yet acked.
    pub fn outstanding(&self) -> u32 {
        self.outstanding
    }
}

#[cfg(test)]
mod tests {
    use crate::FrameCredits;

    #[test]
    fn sending_stops_at_the_limit_until_an_ack() {
        let mut credits = FrameCredits::new(2);
        assert!(credits.try_send());
        assert!(credits.try_send());
        assert!(!credits.try_send());
        credits.ack();
        assert!(credits.try_send());
        assert_eq!(credits.outstanding(), 2);
    }

    #[test]
    fn a_stray_ack_does_not_mint_a_credit() {
        let mut credits = FrameCredits::new(1);
        credits.ack();
        assert!(credits.try_send());
        assert!(!credits.try_send());
    }
}
