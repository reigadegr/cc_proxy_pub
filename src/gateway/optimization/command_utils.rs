const EMPTY_FILEPATHS_XML: &str = "<filepaths>\n</filepaths>";

pub fn extract_command_prefix(command: &str) -> String {
    if command.contains('`') || command.contains("$(") {
        return String::from("command_injection_detected");
    }

    let parts = split_shell_words(command);
    if parts.is_empty() {
        return String::from("none");
    }

    let mut env_prefix = Vec::new();
    let mut command_start = 0_usize;

    for (index, part) in parts.iter().enumerate() {
        if part.contains('=') && !part.starts_with('-') {
            env_prefix.push(part.clone());
            command_start = index + 1;
        } else {
            break;
        }
    }

    if command_start >= parts.len() {
        return String::from("none");
    }

    let command_parts = &parts[command_start..];
    let first_word = command_parts[0].as_str();

    if is_two_word_command(first_word) && command_parts.len() > 1 {
        let second_word = command_parts[1].as_str();
        if !second_word.starts_with('-') {
            return format!("{first_word} {second_word}");
        }
        return first_word.to_owned();
    }

    if env_prefix.is_empty() {
        first_word.to_owned()
    } else {
        format!("{} {first_word}", env_prefix.join(" "))
    }
}

pub fn extract_filepaths_from_command(command: &str, _output: &str) -> String {
    let parts = split_shell_words(command);
    if parts.is_empty() {
        return String::from(EMPTY_FILEPATHS_XML);
    }

    let base_command = parts[0]
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(&parts[0])
        .to_ascii_lowercase();

    if is_listing_command(base_command.as_str()) {
        return String::from(EMPTY_FILEPATHS_XML);
    }

    if is_reading_command(base_command.as_str()) {
        let filepaths: Vec<&str> = parts
            .iter()
            .skip(1)
            .map(String::as_str)
            .filter(|part| !part.starts_with('-'))
            .collect();

        return build_filepaths_xml(&filepaths);
    }

    if base_command == "grep" {
        let mut pattern_provided_via_flag = false;
        let mut positional = Vec::new();
        let mut skip_next = false;

        for part in parts.iter().skip(1) {
            if skip_next {
                skip_next = false;
                continue;
            }

            if part.starts_with('-') {
                if is_flag_with_argument(part) {
                    if part == "-e" || part == "-f" {
                        pattern_provided_via_flag = true;
                    }
                    skip_next = true;
                }
                continue;
            }

            positional.push(part.as_str());
        }

        let filepaths = if pattern_provided_via_flag {
            positional
        } else {
            positional.into_iter().skip(1).collect()
        };

        return build_filepaths_xml(&filepaths);
    }

    String::from(EMPTY_FILEPATHS_XML)
}

fn is_two_word_command(command: &str) -> bool {
    matches!(
        command,
        "git" | "npm" | "docker" | "kubectl" | "cargo" | "go" | "pip" | "yarn"
    )
}

fn is_listing_command(command: &str) -> bool {
    matches!(
        command,
        "ls" | "dir" | "find" | "tree" | "pwd" | "cd" | "mkdir" | "rmdir" | "rm"
    )
}

fn is_reading_command(command: &str) -> bool {
    matches!(
        command,
        "cat" | "head" | "tail" | "less" | "more" | "bat" | "type"
    )
}

fn is_flag_with_argument(flag: &str) -> bool {
    matches!(flag, "-e" | "-f" | "-m" | "-A" | "-B" | "-C")
}

fn build_filepaths_xml(filepaths: &[&str]) -> String {
    if filepaths.is_empty() {
        return String::from(EMPTY_FILEPATHS_XML);
    }

    format!("<filepaths>\n{}\n</filepaths>", filepaths.join("\n"))
}

fn split_shell_words(input: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaping = false;

    for ch in input.chars() {
        if escaping {
            current.push(ch);
            escaping = false;
            continue;
        }

        if ch == '\\' && !in_single_quote {
            escaping = true;
            continue;
        }

        if ch == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            continue;
        }

        if ch == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            continue;
        }

        if ch.is_whitespace() && !in_single_quote && !in_double_quote {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }

        current.push(ch);
    }

    if escaping {
        current.push('\\');
    }

    if !current.is_empty() {
        words.push(current);
    }

    words
}

#[cfg(test)]
mod tests {
    use super::{extract_command_prefix, extract_filepaths_from_command};

    #[test]
    fn test_extract_command_prefix() {
        assert_eq!(extract_command_prefix("git commit -m hi"), "git commit");
        assert_eq!(extract_command_prefix("npm --version"), "npm");
        assert_eq!(
            extract_command_prefix("FOO=1 BAR=2 git status"),
            "git status"
        );
        assert_eq!(
            extract_command_prefix("FOO=1 BAR=2 python run.py"),
            "FOO=1 BAR=2 python"
        );
        assert_eq!(
            extract_command_prefix("echo $(cat /tmp/a)"),
            "command_injection_detected"
        );
        assert_eq!(extract_command_prefix(""), "none");
    }

    #[test]
    fn test_extract_filepaths_from_command() {
        assert_eq!(
            extract_filepaths_from_command("ls -la", ""),
            "<filepaths>\n</filepaths>"
        );
        assert_eq!(
            extract_filepaths_from_command("cat -n foo.txt bar.md", ""),
            "<filepaths>\nfoo.txt\nbar.md\n</filepaths>"
        );
        assert_eq!(
            extract_filepaths_from_command("grep pattern file1.txt file2.txt", ""),
            "<filepaths>\nfile1.txt\nfile2.txt\n</filepaths>"
        );
        assert_eq!(
            extract_filepaths_from_command("grep -e pattern file.txt", ""),
            "<filepaths>\nfile.txt\n</filepaths>"
        );
    }
}
