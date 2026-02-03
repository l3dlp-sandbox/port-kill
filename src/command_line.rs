pub fn parse_command_line(command_line: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote_char: Option<char> = None;

    for c in command_line.chars() {
        match c {
            '"' | '\'' => match quote_char {
                None => quote_char = Some(c),
                Some(q) if q == c => quote_char = None,
                Some(_) => current.push(c),
            },
            ' ' | '\t' if quote_char.is_none() => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            _ => {
                current.push(c);
            }
        }
    }

    if !current.is_empty() {
        parts.push(current);
    }

    parts
}

#[cfg(test)]
mod tests {
    use super::parse_command_line;

    #[test]
    fn test_parse_command_line() {
        let cmd = "npm run dev --port 3000";
        let parts = parse_command_line(cmd);
        assert_eq!(parts, vec!["npm", "run", "dev", "--port", "3000"]);
    }

    #[test]
    fn test_parse_command_line_with_quotes() {
        let cmd = r#"node "my script.js" --arg "value with spaces""#;
        let parts = parse_command_line(cmd);
        assert_eq!(
            parts,
            vec!["node", "my script.js", "--arg", "value with spaces"]
        );
    }

    #[test]
    fn test_parse_command_line_with_mixed_quotes() {
        let cmd = r#"echo "it's working" --arg test"#;
        let parts = parse_command_line(cmd);
        assert_eq!(parts, vec!["echo", "it's working", "--arg", "test"]);
    }

    #[test]
    fn test_parse_command_line_with_apostrophe_in_filename() {
        let cmd = r#"node "user's script.js" --port 3000"#;
        let parts = parse_command_line(cmd);
        assert_eq!(parts, vec!["node", "user's script.js", "--port", "3000"]);
    }
}
