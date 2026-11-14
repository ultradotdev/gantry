//! How the commands that talk someone through a job lay out what they
//! print: a heading per step, a tick for what is done, and a frame around
//! a command that is theirs to copy and run. Styled on a terminal, plain
//! in a file or a pipe.

use std::io::IsTerminal;

/// The width of headings and frames. Narrow enough for any terminal.
const WIDTH: usize = 64;

fn styled() -> bool {
    std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

fn paint(code: &str, text: &str) -> String {
    if styled() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
}

pub fn bold(text: &str) -> String {
    paint("1", text)
}

/// A line saying something is done.
pub fn done(text: &str) -> String {
    format!("{} {text}", paint("32", "✓"))
}

/// `── label ─────`, filled out to the common width.
fn rule(label: &str) -> String {
    let lead = if label.is_empty() { String::new() } else { format!("── {label} ") };
    let rest = WIDTH.saturating_sub(lead.chars().count());
    format!("{lead}{}", "─".repeat(rest))
}

/// The line a step opens with.
pub fn heading(title: &str) -> String {
    bold(&rule(title))
}

/// A command for the reader to copy, set apart between two lines. The
/// command starts at the margin and the frame has no sides, so that
/// selecting it, however it wraps, picks up nothing but the command.
pub fn copy_block(command: &str) -> String {
    format!("{}\n\n{command}\n\n{}", paint("2", &rule("copy this")), paint("2", &rule("")))
}

/// `3 cameras`, `1 light`.
pub fn count(number: usize, thing: &str) -> String {
    format!("{number} {thing}{}", if number == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_to_copy_stands_alone_between_two_lines() {
        // Tests do not run on a terminal, so nothing is styled.
        let block = copy_block("sudo /bin/true && sudo /bin/false");
        let lines: Vec<&str> = block.lines().collect();
        assert_eq!(lines.len(), 5, "{block}");
        assert!(lines[0].starts_with("── copy this ─") && lines[4].chars().all(|c| c == '─'), "{block}");
        assert_eq!((lines[1], lines[2], lines[3]), ("", "sudo /bin/true && sudo /bin/false", ""));
        assert_eq!(lines[0].chars().count(), lines[4].chars().count());
    }

    #[test]
    fn headings_share_a_width_and_counts_agree_in_number() {
        assert_eq!(heading("Step 2 of 4 · Sign in").chars().count(), WIDTH);
        assert!(heading("Step 2 of 4 · Sign in").starts_with("── Step 2 of 4 · Sign in ─"));
        assert_eq!(done("Saved."), "✓ Saved.");
        assert_eq!((count(1, "light"), count(0, "sensor"), count(15, "camera")), ("1 light".into(), "0 sensors".into(), "15 cameras".into()));
    }
}
