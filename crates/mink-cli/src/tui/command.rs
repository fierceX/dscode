#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SlashCommand {
    Flash,
    Pro,
    Model(String),
    Compact,
    Help,
    Skills,
    Quit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SlashCommandError {
    pub input: String,
}

pub(crate) fn parse_slash_command(input: &str) -> Result<Option<SlashCommand>, SlashCommandError> {
    if !input.starts_with('/') {
        return Ok(None);
    }

    let command = match input {
        "/flash" => SlashCommand::Flash,
        "/pro" => SlashCommand::Pro,
        "/compact" => SlashCommand::Compact,
        "/help" => SlashCommand::Help,
        "/skills" => SlashCommand::Skills,
        "/exit" | "/quit" | "/q" => SlashCommand::Quit,
        _ if input.starts_with("/model ") && !input["/model ".len()..].trim().is_empty() => {
            SlashCommand::Model(input["/model ".len()..].trim().to_string())
        }
        _ => {
            return Err(SlashCommandError {
                input: input.to_string(),
            });
        }
    };

    Ok(Some(command))
}
