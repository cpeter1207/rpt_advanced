//! Validate an ICE-only re-offer before touching the existing DTLS association.

use super::{Candidate, ConnectionState, IceConnectionState, UdpDriver, invalid_transport, io};
use crate::working_files::immutable::artifact_sha256;
use ring::digest::{Context, SHA256};
use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use zeroize::{Zeroize, Zeroizing};

/// Private, value-free comparison state for an already authorized remote offer.
pub struct IceRestartBasis {
    ufrag: [u8; 32],
    password: [u8; 32],
    contract: [u8; 32],
}

impl IceRestartBasis {
    /// Capture the ICE credentials and non-ICE SDP contract before accepting this offer.
    pub fn from_offer(offer: &SdpOffer) -> io::Result<Self> {
        // Match str0m's effective credential precedence.
        let mut credentials = offer
            .session
            .ice_creds()
            .or_else(|| offer.media_lines.iter().find_map(|line| line.ice_creds()))
            .ok_or_else(invalid_transport)?;
        let ufrag = artifact_sha256(credentials.ufrag.as_bytes());
        let password = artifact_sha256(credentials.pass.as_bytes());
        credentials.ufrag.zeroize();
        credentials.pass.zeroize();

        let text = Zeroizing::new(offer.to_sdp_string());
        let mut contract = Context::new(&SHA256);
        let mut data_channel = false;
        for line in text.lines() {
            if line.starts_with("m=") {
                data_channel = line.starts_with("m=application ")
                    && line.ends_with(" UDP/DTLS/SCTP webrtc-datachannel");
            }
            // Firefox can echo the RTP extension format flag on its SCTP section.
            if ice_only_line(line) || (data_channel && line == "a=extmap-allow-mixed") {
                continue;
            }
            contract.update(line.as_bytes());
            contract.update(b"\n");
        }
        Ok(Self {
            ufrag,
            password,
            contract: contract
                .finish()
                .as_ref()
                .try_into()
                .expect("SHA-256 digest size"),
        })
    }
}

fn ice_only_line(line: &str) -> bool {
    // The parsed SDP already canonicalizes a live m-line's port to 9. Keep its
    // disabled state and all other media/DTLS/SCTP attributes in the contract.
    matches!(line, "a=end-of-candidates")
        || [
            "o=",
            "c=",
            "a=ice-ufrag:",
            "a=ice-pwd:",
            "a=candidate:",
            "a=remote-candidates:",
            "a=rtcp:",
        ]
        .iter()
        .any(|prefix| line.starts_with(prefix))
}

impl UdpDriver {
    /// Accept an already authorized ICE-only re-offer on this same association.
    ///
    /// Retains DTLS, SSRCs, data channels and bound sockets. Require fresh ICE
    /// nomination before resuming station media. The caller owns media grant
    /// retirement and must stop old-generation work before invoking this.
    pub fn accept_restart_offer(
        &mut self,
        basis: &mut IceRestartBasis,
        offer: SdpOffer,
    ) -> io::Result<SdpAnswer> {
        let next = IceRestartBasis::from_offer(&offer)?;
        if basis.ufrag == next.ufrag
            || basis.password == next.password
            || basis.contract != next.contract
            || (self.direct.is_none() && self.relay.is_none())
        {
            return Err(invalid_transport());
        }

        // str0m otherwise keeps the old pinned fingerprint silently on a re-offer.
        if self.connection.dtls_authenticated {
            let fingerprint = offer
                .session
                .fingerprint()
                .or_else(|| offer.media_lines.iter().find_map(|line| line.fingerprint()));
            if fingerprint.as_ref() != self.rtc.direct_api().remote_dtls_fingerprint() {
                return Err(invalid_transport());
            }
        }

        self.connection.ice = IceConnectionState::Checking;
        let answer = self.rtc.sdp_api().accept_offer(offer).map_err(|_| {
            self.failed_restart();
            invalid_transport()
        })?;
        self.refresh_local_candidates()?;

        self.initial_tick = true;
        self.restart_remote_ufrag = Some(next.ufrag);
        *basis = next;
        Ok(answer)
    }

    /// Request fresh ICE credentials while preserving the negotiated media and DCEP channels.
    pub fn restart_offer(&mut self) -> io::Result<(SdpOffer, SdpPendingOffer)> {
        if !self.connection.secure_connected() {
            return Err(invalid_transport());
        }
        let mut change = self.rtc.sdp_api();
        change.ice_restart(true);
        change.apply().ok_or_else(invalid_transport)
    }

    /// Apply the authenticated peer's answer to the currently pending ICE restart.
    pub fn accept_restart_answer(
        &mut self,
        pending: SdpPendingOffer,
        answer: SdpAnswer,
    ) -> io::Result<()> {
        let fingerprint = answer.session.fingerprint().or_else(|| {
            answer
                .media_lines
                .iter()
                .find_map(|line| line.fingerprint())
        });
        if fingerprint.is_none()
            || fingerprint.as_ref() != self.rtc.direct_api().remote_dtls_fingerprint()
        {
            return Err(invalid_transport());
        }
        let mut remote = answer
            .session
            .ice_creds()
            .or_else(|| answer.media_lines.iter().find_map(|line| line.ice_creds()))
            .ok_or_else(invalid_transport)?;
        let remote_ufrag = artifact_sha256(remote.ufrag.as_bytes());
        remote.ufrag.zeroize();
        remote.pass.zeroize();
        let mut previous = self.rtc.direct_api().local_ice_credentials();
        let previous_ufrag = artifact_sha256(previous.ufrag.as_bytes());
        previous.ufrag.zeroize();
        previous.pass.zeroize();
        self.connection.ice = IceConnectionState::Checking;
        self.rtc
            .sdp_api()
            .accept_answer(pending, answer)
            .map_err(|_| {
                self.failed_restart();
                invalid_transport()
            })?;
        if self.refresh_local_candidates()? == previous_ufrag {
            self.failed_restart();
            return Err(invalid_transport());
        }
        self.initial_tick = true;
        self.restart_remote_ufrag = Some(remote_ufrag);
        Ok(())
    }

    fn failed_restart(&mut self) {
        // Applying a re-offer can mutate ICE state before a later check fails.
        self.rtc.disconnect();
        self.connection = ConnectionState::LOST;
        self.channels = [None; 2];
    }

    fn refresh_local_candidates(&mut self) -> io::Result<[u8; 32]> {
        let mut credentials = self.rtc.direct_api().local_ice_credentials();
        credentials.pass.zeroize();
        let ufrag = Zeroizing::new(credentials.ufrag);
        if let Some(leg) = &mut self.direct {
            let mut candidate =
                Candidate::from_sdp_string(&leg.candidate).map_err(|_| invalid_transport())?;
            candidate.set_ufrag(&ufrag);
            leg.candidate = candidate.to_sdp_string();
        }
        if let Some(relay) = &mut self.relay {
            relay.candidate.set_ufrag(&ufrag);
            relay.candidate_text = relay.candidate.to_sdp_string();
        }
        Ok(artifact_sha256(ufrag.as_bytes()))
    }
}
