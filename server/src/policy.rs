//! Outreach safety rules, as pure functions so they are easy to test and cannot
//! be skipped by a screen. Every send path must call `may_send` first.

use crate::domain::Channel;

/// Everything the send decision depends on, gathered by the caller.
#[derive(Debug, Clone, Copy)]
pub struct SendCheck {
    pub channel: Channel,
    pub person_opted_out: bool,
    pub on_do_not_contact_list: bool,
    pub org_sending_paused: bool,
    pub sequence_approved: bool,
    /// Any inbound touch on any channel since the sequence started, including a
    /// reply the resourcer marked by hand on LinkedIn or WhatsApp.
    pub replied_any_channel: bool,
    /// For email only: is the address a personal one?
    pub is_personal_email: bool,
    /// Has anyone on the team spoken to this person before?
    pub previously_spoken: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Blocked {
    OptedOut,
    DoNotContact,
    SendingPaused,
    NotApproved,
    AlreadyReplied,
    PersonalEmailWithoutRelationship,
}

pub fn may_send(c: SendCheck) -> Result<(), Blocked> {
    if c.person_opted_out {
        return Err(Blocked::OptedOut);
    }
    if c.on_do_not_contact_list {
        return Err(Blocked::DoNotContact);
    }
    if c.org_sending_paused {
        return Err(Blocked::SendingPaused);
    }
    if !c.sequence_approved {
        return Err(Blocked::NotApproved);
    }
    if c.replied_any_channel {
        return Err(Blocked::AlreadyReplied);
    }
    if matches!(c.channel, Channel::Email) && c.is_personal_email && !c.previously_spoken {
        return Err(Blocked::PersonalEmailWithoutRelationship);
    }
    Ok(())
}

/// Normalise an identifier for the do-not-contact list so matching is reliable.
pub fn normalise_identifier(raw: &str) -> String {
    let s = raw.trim().to_lowercase();
    let s = s
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.");
    s.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok() -> SendCheck {
        SendCheck {
            channel: Channel::Email,
            person_opted_out: false,
            on_do_not_contact_list: false,
            org_sending_paused: false,
            sequence_approved: true,
            replied_any_channel: false,
            is_personal_email: false,
            previously_spoken: false,
        }
    }

    #[test]
    fn approved_work_email_can_send() {
        assert_eq!(may_send(ok()), Ok(()));
    }

    #[test]
    fn opt_out_and_dnc_always_block() {
        assert_eq!(
            may_send(SendCheck {
                person_opted_out: true,
                ..ok()
            }),
            Err(Blocked::OptedOut)
        );
        assert_eq!(
            may_send(SendCheck {
                on_do_not_contact_list: true,
                ..ok()
            }),
            Err(Blocked::DoNotContact)
        );
    }

    #[test]
    fn kill_switch_blocks() {
        assert_eq!(
            may_send(SendCheck {
                org_sending_paused: true,
                ..ok()
            }),
            Err(Blocked::SendingPaused)
        );
    }

    #[test]
    fn no_follow_up_after_reply_on_another_channel() {
        assert_eq!(
            may_send(SendCheck {
                replied_any_channel: true,
                ..ok()
            }),
            Err(Blocked::AlreadyReplied)
        );
    }

    #[test]
    fn unapproved_sequence_blocks() {
        assert_eq!(
            may_send(SendCheck {
                sequence_approved: false,
                ..ok()
            }),
            Err(Blocked::NotApproved)
        );
    }

    #[test]
    fn personal_email_needs_prior_relationship() {
        let personal = SendCheck {
            is_personal_email: true,
            ..ok()
        };
        assert_eq!(
            may_send(personal),
            Err(Blocked::PersonalEmailWithoutRelationship)
        );
        assert_eq!(
            may_send(SendCheck {
                previously_spoken: true,
                ..personal
            }),
            Ok(())
        );
    }

    #[test]
    fn identifiers_normalise() {
        assert_eq!(
            normalise_identifier(" https://www.LinkedIn.com/in/Jane-Doe/ "),
            "linkedin.com/in/jane-doe"
        );
        assert_eq!(normalise_identifier("Jane@Example.com"), "jane@example.com");
    }
}
