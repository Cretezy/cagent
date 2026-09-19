use std::io::{self, Write};
use std::time::{Duration, Instant};

pub(super) const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(1);
const PROGRESS_ACTIVE: &str = "\x1b]9;4;3\x07";
const PROGRESS_CLEAR: &str = "\x1b]9;4;0\x07";
const TERMINAL_BELL: &[u8] = b"\x07";
const OSC777_TERMINATOR: &str = "\x1b\\";

pub(super) struct ProgressGuard {
    active: bool,
    last_emission: Option<Instant>,
}

pub(super) async fn wait_for_keepalive(active: bool) {
    if active {
        tokio::time::sleep(KEEPALIVE_INTERVAL).await;
    } else {
        std::future::pending::<()>().await;
    }
}

impl ProgressGuard {
    pub(super) const fn new() -> Self {
        Self {
            active: false,
            last_emission: None,
        }
    }

    pub(super) fn refresh(&mut self, active: bool, enabled: bool) -> io::Result<()> {
        if !enabled {
            if self.active {
                write_sequence(PROGRESS_CLEAR)?;
                self.active = false;
                self.last_emission = None;
            }
            return Ok(());
        }
        if active {
            let keepalive_due = self
                .last_emission
                .is_some_and(|last| last.elapsed() >= KEEPALIVE_INTERVAL);
            if !self.active || keepalive_due {
                write_sequence(PROGRESS_ACTIVE)?;
                self.active = true;
                self.last_emission = Some(Instant::now());
            }
        } else if self.active {
            write_sequence(PROGRESS_CLEAR)?;
            self.active = false;
            self.last_emission = None;
        }
        Ok(())
    }
}

impl Drop for ProgressGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = write_sequence(PROGRESS_CLEAR);
            self.active = false;
        }
    }
}

fn write_sequence(sequence: &str) -> io::Result<()> {
    let mut stdout = io::stdout();
    stdout.write_all(sequence.as_bytes())?;
    stdout.flush()
}

pub(super) fn write_notification(
    mode: cagent_agent::config::BellMethod,
    title: &str,
    description: &str,
) -> io::Result<()> {
    let mut stdout = io::stdout();
    write_notification_to(&mut stdout, mode, title, description)
}

fn write_notification_to<W: Write>(
    writer: &mut W,
    mode: cagent_agent::config::BellMethod,
    title: &str,
    description: &str,
) -> io::Result<()> {
    match mode {
        cagent_agent::config::BellMethod::Bell => {
            writer.write_all(TERMINAL_BELL)?;
        }
        cagent_agent::config::BellMethod::Osc777 => {
            let title = sanitize_notification_text(title);
            let description = sanitize_notification_text(description);
            let sequence = format!("\x1b]777;notify;{title};{description}{OSC777_TERMINATOR}");
            writer.write_all(sequence.as_bytes())?;
        }
    }
    writer.flush()
}

fn sanitize_notification_text(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .map(|character| if character == ';' { ',' } else { character })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{PROGRESS_ACTIVE, PROGRESS_CLEAR, write_notification_to};
    use cagent_agent::config::BellMethod;
    use std::io::Cursor;

    #[test]
    fn progress_sequences_use_osc_9_4() {
        assert_eq!(PROGRESS_ACTIVE, "\x1b]9;4;3\x07");
        assert_eq!(PROGRESS_CLEAR, "\x1b]9;4;0\x07");
    }

    #[test]
    fn bell_notification_writes_exactly_one_bel() {
        let mut output = Cursor::new(Vec::new());

        write_notification_to(&mut output, BellMethod::Bell, "Title", "Body").unwrap();

        assert_eq!(output.into_inner(), b"\x07");
    }

    #[test]
    fn osc777_notification_writes_title_and_description() {
        let mut output = Cursor::new(Vec::new());

        write_notification_to(&mut output, BellMethod::Osc777, "Title", "Body").unwrap();

        assert_eq!(output.into_inner(), b"\x1b]777;notify;Title;Body\x1b\\");
    }

    #[test]
    fn osc777_notification_sanitizes_control_and_separator_characters() {
        let mut output = Cursor::new(Vec::new());

        write_notification_to(&mut output, BellMethod::Osc777, "Ti;tle\n", "Bo\x07dy").unwrap();

        assert_eq!(output.into_inner(), b"\x1b]777;notify;Ti,tle;Body\x1b\\");
    }
}
