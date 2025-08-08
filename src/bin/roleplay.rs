use std::io::Write;

use arrrg::CommandLine;
use rustyline::completion::{Candidate, Completer};
use rustyline::config::EditMode;
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::{Hint, Hinter, HistoryHinter};
use rustyline::{
    Cmd, CompletionType, ConditionalEventHandler, Config, Context, Editor, Event, EventContext,
    EventHandler, Helper, KeyEvent, RepeatCount, Validator,
};
use utf8path::Path;
use yammer::{ChatMessage, ChatRequest, Error, Parameters, Spinner, WordWrap};

//////////////////////////////////////////// CommandHint ///////////////////////////////////////////

#[derive(Clone, Hash, Debug, PartialEq, Eq)]
pub struct CommandHint {
    pub display: String,
    pub complete_up_to: usize,
}

impl CommandHint {
    pub fn new(text: &str, complete_up_to: &str) -> Self {
        assert!(text.starts_with(complete_up_to));
        Self {
            display: text.to_string(),
            complete_up_to: complete_up_to.len(),
        }
    }

    pub fn suffix(&self, strip_chars: usize) -> Self {
        Self {
            display: self.display[strip_chars..].to_owned(),
            complete_up_to: self.complete_up_to.saturating_sub(strip_chars),
        }
    }
}

impl Candidate for CommandHint {
    fn display(&self) -> &str {
        self.display.as_str()
    }

    fn replacement(&self) -> &str {
        self.display.as_str()
    }
}

impl Hint for CommandHint {
    fn display(&self) -> &str {
        &self.display
    }

    fn completion(&self) -> Option<&str> {
        if self.complete_up_to > 0 {
            Some(&self.display[..self.complete_up_to])
        } else {
            None
        }
    }
}

//////////////////////////////////////////// ShellHelper ///////////////////////////////////////////

#[derive(Helper, Validator)]
pub struct ShellHelper {
    pub commands: Vec<CommandHint>,
    #[rustyline(Hinter)]
    pub hinter: HistoryHinter,
    pub hints: Vec<CommandHint>,
}

impl Completer for ShellHelper {
    type Candidate = CommandHint;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _: &Context<'_>,
    ) -> Result<(usize, Vec<CommandHint>), ReadlineError> {
        let candidates = self
            .commands
            .iter()
            .filter_map(|hint| {
                if hint.display.starts_with(&line[..pos]) {
                    Some(hint.suffix(pos))
                } else {
                    None
                }
            })
            .collect();
        Ok((pos, candidates))
    }
}

impl Highlighter for ShellHelper {}

impl Hinter for ShellHelper {
    type Hint = CommandHint;

    fn hint(&self, line: &str, pos: usize, ctx: &Context<'_>) -> Option<CommandHint> {
        if line.is_empty() || pos < line.len() {
            return None;
        }

        if let Some(hint) = self.hinter.hint(line, pos, ctx) {
            return Some(CommandHint::new(&hint, &hint));
        }

        self.hints
            .iter()
            .filter_map(|hint| {
                if hint.display.starts_with(line) {
                    Some(hint.suffix(pos))
                } else {
                    None
                }
            })
            .next()
    }
}

////////////////////////////////////////// TabEventHandler /////////////////////////////////////////

pub struct TabEventHandler;

impl ConditionalEventHandler for TabEventHandler {
    fn handle(&self, _: &Event, n: RepeatCount, _: bool, ctx: &EventContext) -> Option<Cmd> {
        if ctx.line()[..ctx.pos()]
            .chars()
            .next_back()
            .filter(|c| c.is_whitespace())
            .is_some()
        {
            Some(Cmd::SelfInsert(n, '\t'))
        } else {
            None // default complete
        }
    }
}

//////////////////////////////////////////// ChatOptions ///////////////////////////////////////////

/// Options for the `chat` command.
#[derive(
    Clone, Debug, Eq, PartialEq, arrrg_derive::CommandLine, serde::Deserialize, serde::Serialize,
)]
pub struct ChatOptions {
    /// The host to connect to.
    #[arrrg(optional, "The host to connect to.")]
    pub ollama_host: Option<String>,
    /// The model to use from the ollama library.
    #[arrrg(optional, "The model to use from the ollama library.")]
    pub model: String,
    /// The save file for scenario.
    #[arrrg(optional, "The model to use from the ollama library.")]
    pub save: String,
    /// The parameters to pass to the model.
    #[arrrg(nested)]
    pub param: Parameters,
}

impl Default for ChatOptions {
    fn default() -> Self {
        Self {
            ollama_host: None,
            model: "mistral-small:24b-3.1-instruct-2503-fp16".to_string(),
            save: ".ipomrawh.save".to_string(),
            param: Parameters::default(),
        }
    }
}

/////////////////////////////////////////////// Chat ///////////////////////////////////////////////

/// The `chat` command.
#[derive(Clone, Debug)]
pub struct Chat {
    messages: Vec<ChatMessage>,
    options: ChatOptions,
}

impl Chat {
    /// Create a new chat from a changelog file.  Options will only be used if changelog is None.
    pub fn new(options: ChatOptions, system: Option<String>) -> Result<Self, Error> {
        if let Some(system) = system {
            let messages = vec![ChatMessage {
                role: "system".to_string(),
                content: system,
                images: None,
                tool_calls: None,
            }];
            Ok(Self { messages, options })
        } else {
            let messages = vec![];
            Ok(Self { messages, options })
        }
    }

    /// Assemble the assistant response into a ChatMessage from the pieces.
    fn assemble_assistant_response(pieces: Vec<serde_json::Value>) -> ChatMessage {
        let content = pieces
            .into_iter()
            .flat_map(|x| {
                if let Some(serde_json::Value::Object(x)) = x.get("message") {
                    if let Some(serde_json::Value::String(x)) = x.get("content") {
                        Some(x.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("");
        ChatMessage {
            role: "assistant".to_string(),
            content,
            images: None,
            tool_calls: None,
        }
    }

    /// Convert the chat into a chat request.
    fn into_request(self) -> ChatRequest {
        ChatRequest {
            model: self.options.model,
            messages: self.messages,
            stream: Some(true),
            tools: None,
            format: None,
            keep_alive: None,
            options: serde_json::json!({}),
        }
    }

    /// Run the interactive chat shell.
    pub async fn shell(mut self) -> Result<Option<String>, yammer::Error> {
        let config = Config::builder()
            .auto_add_history(true)
            .edit_mode(EditMode::Vi)
            .completion_type(CompletionType::List)
            .check_cursor_position(true)
            .max_history_size(1_000_000)
            .expect("this should always work")
            .history_ignore_dups(true)
            .expect("this should always work")
            .history_ignore_space(true)
            .build();
        let history = rustyline::history::FileHistory::new();
        let mut rl = Editor::with_history(config, history).expect("this should always work");
        const PROMPT: &str = ">>> ";
        let commands = vec![
            CommandHint::new(":help", ":help"),
            CommandHint::new(":exit", ":exit"),
            CommandHint::new(":quit", ":quit"),
            CommandHint::new(":edit", ":edit"),
            CommandHint::new(":retry", ":retry"),
            CommandHint::new(":param", ":param"),
        ];
        let h = ShellHelper {
            commands: commands.clone(),
            hinter: HistoryHinter::new(),
            hints: commands.clone(),
        };
        rl.set_helper(Some(h));
        rl.bind_sequence(
            KeyEvent::from('\t'),
            EventHandler::Conditional(Box::new(TabEventHandler)),
        );
        rl.bind_sequence(KeyEvent::ctrl('l'), EventHandler::Simple(Cmd::ClearScreen));
        loop {
            let line = rl.readline(PROMPT);
            let (args, line) = match line {
                Ok(line) => {
                    let args = match shvar::split(&line) {
                        Ok(args) => args,
                        Err(err) => {
                            eprintln!("could not split line: {err:?}");
                            continue;
                        }
                    };
                    if args.is_empty() {
                        continue;
                    }
                    (args, line)
                }
                Err(ReadlineError::Interrupted) => {
                    continue;
                }
                Err(ReadlineError::Eof) => {
                    break Ok(None);
                }
                Err(err) => {
                    eprintln!("could not read line: {err}");
                    continue;
                }
            };
            let prompt = match args[0].as_str() {
                ":exit" | ":quit" | ":wq" | ":q" => {
                    break Ok(None);
                }
                ":help" => {
                    eprintln!(
                        r#"chat
====

Commands:

:model <model>  Set the model to use.
:edit           Edit and send the next message.
:retry          Retry the last message.

Anything else will be interpreted as a message.
"#
                    );
                    continue;
                }
                ":model" => {
                    if args.len() < 2 {
                        eprintln!("expected model name");
                        continue;
                    }
                    if args.len() > 2 {
                        eprintln!("expected only model name");
                        continue;
                    }
                    self.options.model = args[1].clone();
                    continue;
                }
                ":edit" => {
                    let promptfile = match yammer::editor("Replace this text with your prompt.") {
                        Ok(promptfile) => promptfile.as_ref().clone(),
                        Err(err) => {
                            eprintln!("could not edit: {err:?}");
                            continue;
                        }
                    };
                    let prompt = std::fs::read_to_string(&promptfile)?;
                    std::fs::remove_file(promptfile)?;
                    writeln!(
                        std::io::stdout(),
                        "{}",
                        prompt
                            .split_terminator('\n')
                            .map(|x| "... ".to_string() + x)
                            .collect::<Vec<_>>()
                            .join("\n")
                    )?;
                    prompt
                }
                ":reply" => {
                    let reply_to = if !self.messages.is_empty() {
                        &self.messages[self.messages.len() - 1].content
                    } else {
                        ""
                    };
                    let promptfile = match yammer::editor(reply_to) {
                        Ok(promptfile) => promptfile.as_ref().clone(),
                        Err(err) => {
                            eprintln!("could not edit: {err:?}");
                            continue;
                        }
                    };
                    let prompt = std::fs::read_to_string(&promptfile)?;
                    std::fs::remove_file(promptfile)?;
                    writeln!(
                        std::io::stdout(),
                        "{}",
                        prompt
                            .split_terminator('\n')
                            .map(|x| "... ".to_string() + x)
                            .collect::<Vec<_>>()
                            .join("\n")
                    )?;
                    prompt
                }
                _ => {
                    if !args[0].starts_with(':') {
                        line
                    } else {
                        eprintln!("unknown command: {}", args[0]);
                        continue;
                    }
                }
            };
            self.messages.push(ChatMessage {
                role: "user".to_string(),
                content: prompt,
                images: None,
                tool_calls: None,
            });
            let req = self.clone().into_request();
            let req = req.make_request(&yammer::ollama_host(self.options.ollama_host.clone()));
            let spinner = Spinner::new();
            spinner.start();
            let mut pieces = vec![];
            let mut ww = WordWrap::new(100);
            let res = yammer::stream(req, |resp| {
                spinner.inhibit();
                if let Some(serde_json::Value::Object(message)) = resp.get("message") {
                    if let Some(serde_json::Value::String(content)) = message.get("content") {
                        ww.push(content.clone(), &mut std::io::stdout())?;
                    }
                }
                pieces.push(resp);
                Ok(())
            })
            .await;
            spinner.inhibit();
            println!();
            if let Err(Error::Signal) = res {
                continue;
            } else if let Err(err) = res {
                eprintln!("could not chat: {err:?}");
                continue;
            };
            let message = Self::assemble_assistant_response(pieces);
            const HEADER: &str = "```roleplay\n";
            if let Some(offset) = message.content.find(HEADER) {
                let Some(x) = message.content[offset..].strip_prefix(HEADER) else {
                    continue;
                };
                let Some((role_play, _)) = x.split_once("\n```") else {
                    continue;
                };
                return Ok(Some(role_play.to_string()));
            }
            self.messages.push(message);
        }
    }
}

async fn interview(system: String, options: ChatOptions) -> Result<Option<String>, yammer::Error> {
    let chat = Chat::new(options, Some(system))?;
    chat.shell().await
}

async fn conduct_interviews(
    interviews: &[RolePlayingInterview],
    options: &ChatOptions,
) -> Result<Option<String>, yammer::Error> {
    let mut prompt = String::new();
    for int in interviews {
        println!(
            "\nConducting {} interview now...\n{}",
            int.r#type, int.introduction,
        );
        let system = int.system.join("\n") + &prompt;
        let Some(prompt_piece) = interview(system, options.clone()).await? else {
            eprintln!("expected information about {}; exiting", int.r#type);
            std::process::exit(13);
        };
        prompt += &format!("<{}>\n{prompt_piece}\n</{}>\n", int.xml_tag, int.xml_tag);
    }
    Ok(Some(prompt))
}

struct RolePlayingInterview {
    r#type: String,
    introduction: String,
    system: Vec<String>,
    xml_tag: String,
}

#[tokio::main]
async fn main() -> Result<(), yammer::Error> {
    minimal_signals::block();
    let (options, free) = ChatOptions::from_command_line_relaxed("USAGE: wizard-roleplay");
    if !free.is_empty() {
        eprintln!("command takes no positional arguments");
        std::process::exit(13);
    }
    if Path::from(&options.save).exists() {
        eprintln!("save exists; using previous scenario");
    } else {
        let interviews = [
            RolePlayingInterview {
                r#type: "scenario".to_string(),
                introduction:
                    "This interview is about setting up the scenario for the role playing.

Focus on what happens, not so much the who or how.
"
                    .to_string(),
                system: vec![
                    include_str!("../../prompts/common.md").to_string(),
                    include_str!("../../prompts/scenario.md").to_string(),
                ],
                xml_tag: "scenario".to_string(),
            },
            RolePlayingInterview {
                r#type: "assistant".to_string(),
                introduction: "My ideal assistant...
"
                .to_string(),
                system: vec![
                    include_str!("../../prompts/common.md").to_string(),
                    include_str!("../../prompts/assistant.md").to_string(),
                ],
                xml_tag: "about assistant".to_string(),
            },
            RolePlayingInterview {
                r#type: "user".to_string(),
                introduction: "About me...
"
                .to_string(),
                system: vec![
                    include_str!("../../prompts/common.md").to_string(),
                    include_str!("../../prompts/user.md").to_string(),
                ],
                xml_tag: "about user".to_string(),
            },
            RolePlayingInterview {
                r#type: "us".to_string(),
                introduction: "About us...
"
                .to_string(),
                system: vec![
                    include_str!("../../prompts/common.md").to_string(),
                    include_str!("../../prompts/us.md").to_string(),
                ],
                xml_tag: "about us".to_string(),
            },
            RolePlayingInterview {
                r#type: "rules".to_string(),
                introduction: "Rules for engagement...
"
                .to_string(),
                system: vec![
                    include_str!("../../prompts/common.md").to_string(),
                    include_str!("../../prompts/rules.md").to_string(),
                ],
                xml_tag: "rules".to_string(),
            },
        ];
        if let Some(prompt) = conduct_interviews(&interviews, &options).await? {
            std::fs::write(&options.save, &prompt)?;
        }
    }
    let system = std::fs::read_to_string(&options.save)?;
    println!("\nbeginning roleplay...");
    let chat = Chat::new(options, Some(system))?;
    chat.shell().await?;
    Ok(())
}
