//! The command's arguments as the user edits them: one argument per line.
//!
//! The stored form stays a list of strings, so settings written before this
//! text field existed keep loading. Lines are the only separator: spaces,
//! quotes and everything else in a line belong to its argument, the command is
//! run without a shell, and nothing here splits or unquotes.

/// The text for a list of arguments, one per line, or `None` when a line
/// cannot hold them exactly: an empty argument would vanish (empty lines are
/// not arguments) and a line break inside an argument would split it.
pub(super) fn to_lines(arguments: &[String]) -> Option<String> {
    if arguments
        .iter()
        .any(|argument| argument.is_empty() || argument.contains(['\n', '\r']))
    {
        return None;
    }
    Some(arguments.join("\n"))
}

/// The arguments in `text`: every non-empty line, exactly as written. A
/// trailing carriage return of a Windows line ending is not part of the
/// argument; leading and trailing spaces are.
pub(super) fn from_lines(text: &str) -> Vec<String> {
    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn arguments_round_trip_through_lines() {
        let arguments = list(&["--title", "Among Terminal", "-n", "7"]);
        let text = to_lines(&arguments).unwrap();
        assert_eq!(text, "--title\nAmong Terminal\n-n\n7");
        assert_eq!(from_lines(&text), arguments);
        assert_eq!(to_lines(&[]).as_deref(), Some(""));
        assert!(from_lines("").is_empty());
    }

    #[test]
    fn spaces_and_quotes_stay_inside_their_argument() {
        // No shell and no unquoting: a quote is a character, a space is a
        // character, and only the line break separates arguments.
        let arguments = list(&[
            "--message=a b  c",
            "\"quoted\"",
            "it's",
            "$HOME `date` *",
            " leading and trailing ",
            "\\n",
        ]);
        let text = to_lines(&arguments).unwrap();
        assert_eq!(from_lines(&text), arguments);
        assert_eq!(text.lines().count(), arguments.len());
    }

    #[test]
    fn empty_lines_are_not_arguments() {
        assert_eq!(from_lines("a\n\nb\n\n\n"), list(&["a", "b"]));
        assert_eq!(from_lines("\n\n"), Vec::<String>::new());
        // A line of spaces is a real argument, not an empty line.
        assert_eq!(from_lines("a\n \nb"), list(&["a", " ", "b"]));
    }

    #[test]
    fn a_windows_line_ending_is_not_part_of_the_argument() {
        assert_eq!(from_lines("a\r\nb\r\n"), list(&["a", "b"]));
        assert_eq!(from_lines("a\rb"), list(&["a\rb"]));
    }

    #[test]
    fn arguments_a_line_cannot_hold_are_refused_not_altered() {
        // They stay editable as the JSON list they were saved as.
        assert!(to_lines(&list(&["a", ""])).is_none());
        assert!(to_lines(&list(&["two\nlines"])).is_none());
        assert!(to_lines(&list(&["carriage\rreturn"])).is_none());
    }

    #[test]
    fn an_existing_json_list_loads_unchanged() {
        let stored: Vec<String> = serde_json::from_str(r#"["--flag", "a b", "\"q\""]"#).unwrap();
        let text = to_lines(&stored).unwrap();
        assert_eq!(from_lines(&text), stored);
    }
}
