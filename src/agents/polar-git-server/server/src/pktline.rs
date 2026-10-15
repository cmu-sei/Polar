/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

//! The head of a `git-receive-pack` request: pkt-line ref-update commands, a flush packet, then
//! the pack. The server reads the commands to authorize them before any git process runs.

use thiserror::Error;

/// One command of a push: move `refname` from `old` to `new` (all zeros means absent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefUpdate {
    pub old: String,
    pub new: String,
    pub refname: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PktError {
    #[error("the request ends inside a packet")]
    Truncated,
    #[error("a packet length is not four hex digits")]
    BadLength,
    #[error("a command is not '<old> <new> <ref>'")]
    BadCommand,
    #[error("signed pushes are not supported")]
    PushCert,
    #[error("the request has no commands")]
    Empty,
}

const ZERO_LEN_FLUSH: &[u8] = b"0000";

/// Reads the commands at the start of a receive-pack body. Returns them with the number of bytes
/// they and the flush packet occupy (the pack starts there).
pub fn parse_ref_updates(body: &[u8]) -> Result<(Vec<RefUpdate>, usize), PktError> {
    let mut at = 0;
    let mut updates = Vec::new();
    loop {
        let head = body.get(at..at + 4).ok_or(PktError::Truncated)?;
        if head == ZERO_LEN_FLUSH {
            at += 4;
            break;
        }
        let len = std::str::from_utf8(head)
            .ok()
            .and_then(|s| usize::from_str_radix(s, 16).ok())
            .ok_or(PktError::BadLength)?;
        if len < 4 {
            return Err(PktError::BadLength);
        }
        let payload = body.get(at + 4..at + len).ok_or(PktError::Truncated)?;
        at += len;

        // Capabilities follow a NUL on the first command; shallow lines precede the commands.
        let line = payload.split(|b| *b == 0).next().unwrap_or(payload);
        let line = std::str::from_utf8(line).map_err(|_| PktError::BadCommand)?;
        let line = line.trim_end_matches('\n');
        if line.starts_with("push-cert") {
            return Err(PktError::PushCert);
        }
        if line.starts_with("shallow ") {
            continue;
        }
        let mut parts = line.splitn(3, ' ');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(old), Some(new), Some(refname))
                if is_oid(old) && is_oid(new) && !refname.is_empty() =>
            {
                updates.push(RefUpdate {
                    old: old.to_string(),
                    new: new.to_string(),
                    refname: refname.to_string(),
                });
            }
            _ => return Err(PktError::BadCommand),
        }
    }
    if updates.is_empty() {
        return Err(PktError::Empty);
    }
    Ok((updates, at))
}

fn is_oid(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Frames `payload` as one pkt-line.
pub fn pkt_line(payload: &str) -> Vec<u8> {
    format!("{:04x}{payload}", payload.len() + 4).into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZERO: &str = "0000000000000000000000000000000000000000";
    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn body(lines: &[&str], pack: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for l in lines {
            out.extend(pkt_line(l));
        }
        out.extend(b"0000");
        out.extend(pack);
        out
    }

    #[test]
    fn commands_are_read_and_the_pack_offset_is_returned() {
        let first = format!("{ZERO} {A} refs/heads/main\0 report-status side-band-64k agent=git/2");
        let second = format!("{A} {ZERO} refs/heads/old\n");
        let b = body(&[&first, &second], b"PACKdata");
        let (updates, at) = parse_ref_updates(&b).unwrap();
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].refname, "refs/heads/main");
        assert_eq!(updates[0].old, ZERO);
        assert_eq!(updates[1].refname, "refs/heads/old");
        assert_eq!(&b[at..], b"PACKdata");
    }

    #[test]
    fn a_body_cut_short_is_refused() {
        let first = format!("{ZERO} {A} refs/heads/main");
        let mut b = body(&[&first], b"");
        b.truncate(b.len() - 6);
        assert_eq!(parse_ref_updates(&b), Err(PktError::Truncated));
    }

    #[test]
    fn garbage_is_refused() {
        assert_eq!(parse_ref_updates(b"zzzz"), Err(PktError::BadLength));
        assert_eq!(
            parse_ref_updates(&body(&["not a command"], b"")),
            Err(PktError::BadCommand)
        );
        assert_eq!(parse_ref_updates(b"0000"), Err(PktError::Empty));
    }

    #[test]
    fn signed_pushes_are_refused() {
        assert_eq!(
            parse_ref_updates(&body(&["push-cert\0 caps"], b"")),
            Err(PktError::PushCert)
        );
    }

    #[test]
    fn shallow_lines_are_skipped() {
        let shallow = format!("shallow {A}");
        let cmd = format!("{ZERO} {A} refs/heads/main");
        let (updates, _) = parse_ref_updates(&body(&[&shallow, &cmd], b"")).unwrap();
        assert_eq!(updates.len(), 1);
    }
}
